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

/// Test various communication patterns through the switcher.
#[test]
fn test_communication_patterns() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create a single echo backend
    let backend_dir = temp_dir.path().join("ssh-backend");
    std::fs::create_dir(&backend_dir).expect("Failed to create backend dir");
    let backend_socket = backend_dir.join("agent.test");
    let _backend = spawn_backend(&backend_socket, BackendType::Echo);

    let switcher_socket = temp_dir.path().join("switcher.sock");

    // Start switcher
    let mut child = Command::new(binary_path())
        .arg("--socket-path")
        .arg(&switcher_socket)
        .arg("--agents-dirs")
        .arg(temp_dir.path())
        .spawn()
        .expect("Failed to start ssh-agent-switcher");

    assert!(
        wait_for_path(&switcher_socket, Duration::from_secs(5)),
        "Switcher socket was not created"
    );

    // Helper to create a connected stream
    let connect = || {
        let stream = UnixStream::connect(&switcher_socket).expect("Failed to connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    };

    // Test 1: Single byte write and read
    {
        let mut stream = connect();
        stream.write_all(&[0x42]).expect("Failed to write 1 byte");
        let mut buf = [0u8; 1];
        stream.read_exact(&mut buf).expect("Failed to read 1 byte");
        assert_eq!(buf[0], 0x42, "Single byte echo failed");
    }

    // Test 2: Small writes and reads (various sizes)
    for size in [1, 2, 3, 4, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256] {
        let mut stream = connect();
        let data: Vec<u8> = (0..size).map(|i| (i & 0xFF) as u8).collect();
        stream
            .write_all(&data)
            .unwrap_or_else(|e| panic!("Failed to write {size} bytes: {e}"));
        let mut response = vec![0u8; size];
        stream
            .read_exact(&mut response)
            .unwrap_or_else(|e| panic!("Failed to read {size} bytes: {e}"));
        assert_eq!(response, data, "Echo failed for size {size}");
    }

    // Test 3: Larger writes and reads
    for size in [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
        let mut stream = connect();
        let data: Vec<u8> = (0..size).map(|i| (i & 0xFF) as u8).collect();
        stream
            .write_all(&data)
            .unwrap_or_else(|e| panic!("Failed to write {size} bytes: {e}"));
        let mut response = vec![0u8; size];
        stream
            .read_exact(&mut response)
            .unwrap_or_else(|e| panic!("Failed to read {size} bytes: {e}"));
        assert_eq!(response, data, "Echo failed for size {size}");
    }

    // Test 4: Ping-pong - many small exchanges on same connection
    {
        let mut stream = connect();
        for i in 0..100u8 {
            let data = [i, i.wrapping_add(1), i.wrapping_add(2)];
            stream.write_all(&data).expect("Ping-pong write failed");
            let mut response = [0u8; 3];
            stream.read_exact(&mut response).expect("Ping-pong read failed");
            assert_eq!(response, data, "Ping-pong failed at iteration {i}");
        }
    }

    // Test 5: Series of writes, then series of reads (buffered)
    {
        let mut stream = connect();
        let chunk_size = 100;
        let num_chunks = 10;

        // Write all chunks
        for i in 0..num_chunks {
            let data: Vec<u8> = (0..chunk_size).map(|j| ((i * chunk_size + j) & 0xFF) as u8).collect();
            stream.write_all(&data).expect("Buffered write failed");
        }

        // Read all chunks back
        let mut all_response = vec![0u8; chunk_size * num_chunks];
        stream
            .read_exact(&mut all_response)
            .expect("Buffered read failed");

        // Verify
        let expected: Vec<u8> = (0..(chunk_size * num_chunks))
            .map(|i| (i & 0xFF) as u8)
            .collect();
        assert_eq!(all_response, expected, "Buffered echo failed");
    }

    // Test 6: Interleaved writes and reads of different sizes
    {
        let mut stream = connect();
        let sizes = [1, 10, 100, 50, 5, 200, 1, 1, 1, 500];
        for &size in &sizes {
            let data: Vec<u8> = (0..size).map(|i| (i & 0xFF) as u8).collect();
            stream.write_all(&data).expect("Interleaved write failed");
            let mut response = vec![0u8; size];
            stream.read_exact(&mut response).expect("Interleaved read failed");
            assert_eq!(response, data, "Interleaved echo failed for size {size}");
        }
    }

    // Test 7: Multiple concurrent connections
    {
        let handles: Vec<_> = (0..5)
            .map(|conn_id| {
                let socket = switcher_socket.clone();
                thread::spawn(move || {
                    let mut stream = UnixStream::connect(&socket).expect("Failed to connect");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();

                    for i in 0..20u8 {
                        let data = [conn_id as u8, i, conn_id as u8 ^ i];
                        stream.write_all(&data).expect("Concurrent write failed");
                        let mut response = [0u8; 3];
                        stream.read_exact(&mut response).expect("Concurrent read failed");
                        assert_eq!(response, data, "Concurrent echo failed");
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Concurrent test thread panicked");
        }
    }

    // Clean up
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    child.wait().expect("Failed to wait for child");
}
