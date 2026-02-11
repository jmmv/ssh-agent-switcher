use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tempfile::TempDir;

/// Wait for a file/socket to appear, with timeout.
fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Wait for a file/socket to disappear, with timeout.
fn wait_for_path_gone(path: &Path, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if !path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Get the path to the built binary.
fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssh-agent-switcher"))
}

/// A running backend task.
struct BackendTask {
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    _handle: JoinHandle<()>,
}

/// Backend behavior type.
#[derive(Clone, Copy)]
enum BackendType {
    /// Echoes back exactly what it receives.
    Echo,
    /// Returns 'a' for every byte received.
    AlwaysA,
}

/// Spawn a backend server on a Unix socket.
fn spawn_backend(socket_path: &Path, backend_type: BackendType) -> BackendTask {
    let listener = UnixListener::bind(socket_path).expect("Failed to bind backend socket");
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);

    let handle = thread::spawn(move || {
        for stream in listener.incoming() {
            if shutdown_clone.load(Ordering::SeqCst) {
                break;
            }
            match stream {
                Ok(stream) => {
                    thread::spawn(move || handle_backend_connection(stream, backend_type));
                }
                Err(_) => break,
            }
        }
    });

    BackendTask {
        socket_path: socket_path.to_path_buf(),
        shutdown,
        _handle: handle,
    }
}

fn handle_backend_connection(mut stream: UnixStream, backend_type: BackendType) {
    let mut buf = [0u8; 1024];

    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let response: Vec<u8> = match backend_type {
                    BackendType::Echo => buf[..n].to_vec(),
                    BackendType::AlwaysA => vec![b'a'; n],
                };
                if stream.write_all(&response).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

impl Drop for BackendTask {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Connect to wake up the blocking accept()
        let _ = UnixStream::connect(&self.socket_path);
    }
}

/// A controllable backend that can be started and stopped.
struct Backend {
    socket_path: PathBuf,
    backend_type: BackendType,
    task: Option<BackendTask>,
}

impl Backend {
    fn new(socket_path: PathBuf, backend_type: BackendType) -> Self {
        Self {
            socket_path,
            backend_type,
            task: None,
        }
    }

    fn start(&mut self) {
        if self.task.is_some() {
            return;
        }
        self.task = Some(spawn_backend(&self.socket_path, self.backend_type));
    }

    fn stop(&mut self) {
        // Drop the task (triggers shutdown via Drop impl)
        self.task.take();
        // Remove the socket file
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Test environment with two controllable backends.
struct TestEnv {
    _temp_dir: TempDir,
    agents_dir: PathBuf,
    switcher_socket: PathBuf,
    pid_file: PathBuf,
    log_file: PathBuf,
    echo_backend: Backend,
    always_a_backend: Backend,
}

impl TestEnv {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create two agent directory structures.
        // Using ssh-echo and ssh-always-a as subdirectory names (must start with "ssh-").
        let echo_dir = temp_dir.path().join("ssh-echo");
        std::fs::create_dir(&echo_dir).expect("Failed to create echo subdir");
        let echo_socket = echo_dir.join("agent.echo");

        let always_a_dir = temp_dir.path().join("ssh-always-a");
        std::fs::create_dir(&always_a_dir).expect("Failed to create always-a subdir");
        let always_a_socket = always_a_dir.join("agent.always");

        let switcher_socket = temp_dir.path().join("switcher.sock");
        let pid_file = temp_dir.path().join("switcher.pid");
        let log_file = temp_dir.path().join("switcher.log");

        TestEnv {
            agents_dir: temp_dir.path().to_path_buf(),
            switcher_socket,
            pid_file,
            log_file,
            echo_backend: Backend::new(echo_socket, BackendType::Echo),
            always_a_backend: Backend::new(always_a_socket, BackendType::AlwaysA),
            _temp_dir: temp_dir,
        }
    }

    /// Connect to the switcher and exchange data.
    /// Returns the response received.
    fn exchange(&self, data: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut stream = UnixStream::connect(&self.switcher_socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(data)?;
        let mut response = vec![0u8; data.len()];
        stream.read_exact(&mut response)?;
        Ok(response)
    }

    /// Try to connect and exchange data, expecting failure (no backend available).
    fn exchange_should_fail(&self, data: &[u8]) -> bool {
        let stream = match UnixStream::connect(&self.switcher_socket) {
            Ok(s) => s,
            Err(_) => return true, // Connection failed, which is acceptable
        };
        // Set a short timeout
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut stream = stream;
        if stream.write_all(data).is_err() {
            return true;
        }
        let mut response = vec![0u8; data.len()];
        // Should fail or timeout since no backend is available
        stream.read_exact(&mut response).is_err()
    }
}

/// Handle to a running switcher process (foreground or daemon).
enum SwitcherProcess {
    Foreground(Child),
    Daemon(libc::pid_t),
}

impl SwitcherProcess {
    fn send_sigint(&self) {
        let pid = match self {
            SwitcherProcess::Foreground(child) => child.id() as libc::pid_t,
            SwitcherProcess::Daemon(pid) => *pid,
        };
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }
    }

    fn wait_if_foreground(self) {
        if let SwitcherProcess::Foreground(mut child) = self {
            let status = child.wait().expect("Failed to wait for child");
            assert!(status.success(), "Process did not exit successfully after SIGINT");
        }
    }
}

fn start_switcher(env: &TestEnv, daemon: bool) -> SwitcherProcess {
    if daemon {
        let status = Command::new(binary_path())
            .arg("--daemon")
            .arg("--socket-path")
            .arg(&env.switcher_socket)
            .arg("--agents-dirs")
            .arg(&env.agents_dir)
            .arg("--pid-file")
            .arg(&env.pid_file)
            .arg("--log-file")
            .arg(&env.log_file)
            .status()
            .expect("Failed to start ssh-agent-switcher");

        assert!(status.success(), "Daemon parent process should exit successfully");

        let pid_str = std::fs::read_to_string(&env.pid_file).expect("Failed to read PID file");
        let pid: libc::pid_t = pid_str.trim().parse().expect("Failed to parse PID");
        SwitcherProcess::Daemon(pid)
    } else {
        let child = Command::new(binary_path())
            .arg("--socket-path")
            .arg(&env.switcher_socket)
            .arg("--agents-dirs")
            .arg(&env.agents_dir)
            .spawn()
            .expect("Failed to start ssh-agent-switcher");

        SwitcherProcess::Foreground(child)
    }
}

fn run_switcher_test(daemon: bool) {
    let mut env = TestEnv::new();

    // Start with echo backend
    env.echo_backend.start();

    let process = start_switcher(&env, daemon);

    // Wait for the switcher socket to appear
    assert!(
        wait_for_path(&env.switcher_socket, Duration::from_secs(5)),
        "Switcher socket was not created in time"
    );

    // Test 1: Echo backend is active, should echo back
    let test_data = b"Hello, SSH agent!";
    let response = env.exchange(test_data).expect("Failed to exchange with echo backend");
    assert_eq!(&response, test_data, "Echo backend should echo back exactly");

    // Test 2: Stop echo, start always-a backend
    env.echo_backend.stop();
    assert!(
        wait_for_path_gone(&env.echo_backend.socket_path, Duration::from_secs(2)),
        "Echo socket should be removed"
    );
    env.always_a_backend.start();
    assert!(
        wait_for_path(&env.always_a_backend.socket_path, Duration::from_secs(2)),
        "Always-a socket should appear"
    );

    // Should now get 'a's back
    let test_data = b"Hello!";
    let response = env
        .exchange(test_data)
        .expect("Failed to exchange with always-a backend");
    assert_eq!(&response, b"aaaaaa", "Always-a backend should return all 'a's");

    // Test 3: Start echo backend again (both running), should connect to first found
    env.echo_backend.start();
    assert!(
        wait_for_path(&env.echo_backend.socket_path, Duration::from_secs(2)),
        "Echo socket should appear"
    );

    // The switcher searches directories in sorted order, so ssh-always-a comes before ssh-echo
    let test_data = b"Test";
    let response = env.exchange(test_data).expect("Failed to exchange");
    // Should get 'a's since ssh-always-a is sorted before ssh-echo
    assert_eq!(&response, b"aaaa", "Should connect to always-a (sorted first)");

    // Test 4: Stop always-a, should fall back to echo
    env.always_a_backend.stop();
    assert!(
        wait_for_path_gone(&env.always_a_backend.socket_path, Duration::from_secs(2)),
        "Always-a socket should be removed"
    );

    let test_data = b"Fallback test";
    let response = env.exchange(test_data).expect("Failed to exchange with echo backend");
    assert_eq!(&response, test_data, "Should fall back to echo backend");

    // Test 5: Stop all backends, connection should fail
    env.echo_backend.stop();
    assert!(
        wait_for_path_gone(&env.echo_backend.socket_path, Duration::from_secs(2)),
        "Echo socket should be removed"
    );

    assert!(
        env.exchange_should_fail(b"No backend"),
        "Should fail when no backend is available"
    );

    // Test 6: Restart a backend, should work again
    env.echo_backend.start();
    assert!(
        wait_for_path(&env.echo_backend.socket_path, Duration::from_secs(2)),
        "Echo socket should appear"
    );

    let test_data = b"Back online";
    let response = env.exchange(test_data).expect("Failed to exchange after restart");
    assert_eq!(&response, test_data, "Echo backend should work after restart");

    // Clean up: send SIGINT and wait for process to exit
    process.send_sigint();
    process.wait_if_foreground();

    // Verify socket was cleaned up
    assert!(
        wait_for_path_gone(&env.switcher_socket, Duration::from_secs(2)),
        "Switcher socket should be removed after shutdown"
    );

    if daemon {
        assert!(
            wait_for_path_gone(&env.pid_file, Duration::from_secs(2)),
            "PID file should be removed after shutdown"
        );

        // Print daemon log for visual inspection
        if let Ok(log) = std::fs::read_to_string(&env.log_file) {
            println!("=== Daemon log ===\n{log}=== End daemon log ===");
        }
    }
}

#[test]
fn test_foreground_mode() {
    run_switcher_test(false);
}

#[test]
fn test_daemon_mode() {
    run_switcher_test(true);
}
