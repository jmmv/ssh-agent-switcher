//! Integration tests translated from inttest.sh
//!
//! Some tests require `ssh-agent` and `ssh-add` to be available.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssh-agent-switcher"))
}

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

fn send_signal(pid: libc::pid_t, sig: libc::c_int) {
    unsafe {
        libc::kill(pid, sig);
    }
}

fn has_ssh_agent() -> bool {
    Command::new("ssh-agent").arg("-h").output().is_ok()
}

/// Helper to run switcher and get its PID for cleanup
struct SwitcherProcess {
    pid: libc::pid_t,
    socket_path: PathBuf,
}

impl SwitcherProcess {
    fn kill(&self) {
        send_signal(self.pid, libc::SIGTERM);
    }
}

impl Drop for SwitcherProcess {
    fn drop(&mut self) {
        self.kill();
        wait_for_path_gone(&self.socket_path, Duration::from_secs(2));
    }
}

// =============================================================================
// standalone fixture tests
// =============================================================================

#[test]
fn test_default_agents_dirs() {
    let temp_dir = TempDir::new().unwrap();
    let fake_home = temp_dir.path().join("home");
    fs::create_dir(&fake_home).unwrap();

    let output = Command::new(binary_path())
        .arg("-h")
        .env("HOME", &fake_home)
        .env("USER", "fake-user")
        .output()
        .expect("Failed to run ssh-agent-switcher");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected_pattern = format!("{}/.ssh/agent:/tmp", fake_home.display());
    assert!(
        stdout.contains(&expected_pattern),
        "Expected default agents dirs to contain '{}', got: {}",
        expected_pattern,
        stdout
    );
}

#[test]
fn test_default_socket_path() {
    let temp_dir = TempDir::new().unwrap();
    let fake_home = temp_dir.path().join("home");
    fs::create_dir(&fake_home).unwrap();

    let default_socket = PathBuf::from("/tmp/ssh-agent.test-user-default-socket");

    // Clean up any leftover socket from previous failed runs
    let _ = fs::remove_file(&default_socket);

    let mut child = Command::new(binary_path())
        .env("HOME", &fake_home)
        .env("USER", "test-user-default-socket")
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start ssh-agent-switcher");

    assert!(
        wait_for_path(&default_socket, Duration::from_secs(5)),
        "Default socket was not created"
    );

    // Clean up
    send_signal(child.id() as libc::pid_t, libc::SIGTERM);
    child.wait().unwrap();
    wait_for_path_gone(&default_socket, Duration::from_secs(2));
}

#[test]
fn test_ignore_sighup() {
    let temp_dir = TempDir::new().unwrap();
    let socket = temp_dir.path().join("socket");

    let mut child = Command::new(binary_path())
        .arg("--socket-path")
        .arg(&socket)
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start ssh-agent-switcher");

    assert!(
        wait_for_path(&socket, Duration::from_secs(5)),
        "Socket was not created"
    );

    // Send SIGHUP
    send_signal(child.id() as libc::pid_t, libc::SIGHUP);

    // Wait a bit and verify socket still exists (daemon didn't exit)
    thread::sleep(Duration::from_millis(100));
    assert!(socket.exists(), "Daemon exited after SIGHUP - socket was deleted");

    // Clean up
    send_signal(child.id() as libc::pid_t, libc::SIGTERM);
    child.wait().unwrap();
}

// =============================================================================
// integration_pre_openssh_10_1 fixture tests
// =============================================================================

struct PreOpenssh101Env {
    _temp_dir: TempDir,
    _agent_pid: u32,
    _switcher: SwitcherProcess,
    switcher_socket: PathBuf,
    log_file: PathBuf,
}

impl PreOpenssh101Env {
    fn new() -> Option<Self> {
        if !has_ssh_agent() {
            return None;
        }

        let temp_dir = TempDir::new().unwrap();
        let fake_home = temp_dir.path().join("home");
        fs::create_dir(&fake_home).unwrap();

        let sockets_root = temp_dir.path().join("sockets");
        fs::create_dir(&sockets_root).unwrap();

        // Create ssh-zzz directory (sorts last for unknown files test)
        let agent_dir = sockets_root.join("ssh-zzz");
        fs::create_dir(&agent_dir).unwrap();
        let agent_socket = agent_dir.join("agent.bar");

        // Start real ssh-agent
        let output = Command::new("ssh-agent")
            .arg("-a")
            .arg(&agent_socket)
            .output()
            .expect("Failed to start ssh-agent");

        // Parse SSH_AGENT_PID from output (format: "SSH_AGENT_PID=12345; export ...")
        let stdout = String::from_utf8_lossy(&output.stdout);
        let agent_pid: u32 = stdout
            .lines()
            .find(|l| l.starts_with("SSH_AGENT_PID="))
            .and_then(|l| l.strip_prefix("SSH_AGENT_PID="))
            .and_then(|s| s.split(';').next())
            .and_then(|s| s.trim().parse().ok())
            .expect("Failed to parse SSH_AGENT_PID");

        let switcher_socket = sockets_root.join("switcher");
        let log_file = temp_dir.path().join("switcher.log");

        let child = Command::new(binary_path())
            .arg("--socket-path")
            .arg(&switcher_socket)
            .arg("--agents-dirs")
            .arg(&sockets_root)
            .env("HOME", &fake_home)
            .env("RUST_LOG", "trace")
            .stderr(fs::File::create(&log_file).unwrap())
            .spawn()
            .expect("Failed to start ssh-agent-switcher");

        if !wait_for_path(&switcher_socket, Duration::from_secs(5)) {
            send_signal(agent_pid as libc::pid_t, libc::SIGTERM);
            panic!("Switcher socket was not created");
        }

        Some(Self {
            _temp_dir: temp_dir,
            _agent_pid: agent_pid,
            _switcher: SwitcherProcess {
                pid: child.id() as libc::pid_t,
                socket_path: switcher_socket.clone(),
            },
            switcher_socket,
            log_file,
        })
    }

    fn sockets_root(&self) -> PathBuf {
        self.switcher_socket.parent().unwrap().to_path_buf()
    }
}

impl Drop for PreOpenssh101Env {
    fn drop(&mut self) {
        send_signal(self._agent_pid as libc::pid_t, libc::SIGTERM);
    }
}

#[test]
fn test_pre_openssh_10_1_list_identities() {
    let Some(env) = PreOpenssh101Env::new() else {
        eprintln!("Skipping test: ssh-agent not available");
        return;
    };

    let output = Command::new("ssh-add")
        .arg("-l")
        .env("SSH_AUTH_SOCK", &env.switcher_socket)
        .output()
        .expect("Failed to run ssh-add");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);

    assert!(
        combined.to_lowercase().contains("no identities"),
        "Expected 'no identities', got: {}",
        combined
    );
}

#[test]
fn test_pre_openssh_10_1_add_identity() {
    let Some(env) = PreOpenssh101Env::new() else {
        eprintln!("Skipping test: ssh-agent not available");
        return;
    };

    let key_file = env._temp_dir.path().join("id_rsa");

    // Generate a test key
    let status = Command::new("ssh-keygen")
        .args(["-t", "rsa", "-b", "1024", "-N", "", "-f"])
        .arg(&key_file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("Failed to run ssh-keygen");

    assert!(status.success(), "ssh-keygen failed");

    // Add the key
    let output = Command::new("ssh-add")
        .arg(&key_file)
        .env("SSH_AUTH_SOCK", &env.switcher_socket)
        .output()
        .expect("Failed to run ssh-add");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Identity added"),
        "Expected 'Identity added', got: {}",
        stderr
    );
}

#[test]
fn test_pre_openssh_10_1_ignore_unknown_files() {
    let Some(env) = PreOpenssh101Env::new() else {
        eprintln!("Skipping test: ssh-agent not available");
        return;
    };

    let sockets_root = env.sockets_root();

    // Create garbage in the sockets directory
    fs::write(sockets_root.join("file-unknown"), "").unwrap();
    fs::create_dir(sockets_root.join("dir-unknown")).unwrap();
    fs::write(sockets_root.join("ssh-not-a-dir"), "").unwrap();
    fs::create_dir(sockets_root.join("ssh-empty")).unwrap();
    fs::create_dir(sockets_root.join("ssh-foo")).unwrap();
    fs::write(sockets_root.join("ssh-foo/unknown"), "").unwrap();
    fs::create_dir(sockets_root.join("ssh-bar")).unwrap();
    fs::write(sockets_root.join("ssh-bar/agent.not-a-socket"), "").unwrap();

    // Run ssh-add to trigger socket discovery
    let output = Command::new("ssh-add")
        .arg("-l")
        .env("SSH_AUTH_SOCK", &env.switcher_socket)
        .output()
        .expect("Failed to run ssh-add");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.to_lowercase().contains("no identities"),
        "Expected 'no identities', got: {}",
        combined
    );

    // Check log for correct ignore messages
    let log = fs::read_to_string(&env.log_file).unwrap_or_default();

    assert!(
        log.contains("file-unknown") && log.contains("not a directory"),
        "Expected ignore message for file-unknown"
    );
    assert!(
        log.contains("dir-unknown") && log.contains("ssh-"),
        "Expected ignore message for dir-unknown"
    );
    assert!(
        log.contains("ssh-not-a-dir") && log.contains("not a directory"),
        "Expected ignore message for ssh-not-a-dir"
    );
    assert!(
        log.contains("ssh-empty") && log.contains("No socket"),
        "Expected ignore message for ssh-empty"
    );
    assert!(
        log.contains("ssh-foo/unknown") && log.contains("agent"),
        "Expected ignore message for ssh-foo/unknown"
    );
    assert!(
        log.contains("agent.not-a-socket") && log.contains("Cannot connect"),
        "Expected ignore message for agent.not-a-socket"
    );
}

// =============================================================================
// integration_openssh_10_1 fixture tests
// =============================================================================

struct Openssh101Env {
    _temp_dir: TempDir,
    _agent_pid: u32,
    _switcher: SwitcherProcess,
    switcher_socket: PathBuf,
    sockets_root: PathBuf,
    log_file: PathBuf,
}

impl Openssh101Env {
    fn new() -> Option<Self> {
        if !has_ssh_agent() {
            return None;
        }

        let temp_dir = TempDir::new().unwrap();
        let fake_home = temp_dir.path().join("home");
        fs::create_dir(&fake_home).unwrap();

        // OpenSSH 10.1 style: sockets in ~/.ssh/agent/
        let sockets_root = fake_home.join(".ssh/agent");
        fs::create_dir_all(&sockets_root).unwrap();

        // Name sorts last for unknown files test
        let agent_socket = sockets_root.join("zzz.sshd.aaa");

        // Start real ssh-agent
        let output = Command::new("ssh-agent")
            .arg("-a")
            .arg(&agent_socket)
            .output()
            .expect("Failed to start ssh-agent");

        // Parse SSH_AGENT_PID from output (format: "SSH_AGENT_PID=12345; export ...")
        let stdout = String::from_utf8_lossy(&output.stdout);
        let agent_pid: u32 = stdout
            .lines()
            .find(|l| l.starts_with("SSH_AGENT_PID="))
            .and_then(|l| l.strip_prefix("SSH_AGENT_PID="))
            .and_then(|s| s.split(';').next())
            .and_then(|s| s.trim().parse().ok())
            .expect("Failed to parse SSH_AGENT_PID");

        let switcher_socket = sockets_root.join("switcher");
        let log_file = temp_dir.path().join("switcher.log");

        let child = Command::new(binary_path())
            .arg("--socket-path")
            .arg(&switcher_socket)
            .arg("--agents-dirs")
            .arg(format!("/non-existent-1:{}:/non-existent-2", sockets_root.display()))
            .env("HOME", &fake_home)
            .env("RUST_LOG", "trace")
            .stderr(fs::File::create(&log_file).unwrap())
            .spawn()
            .expect("Failed to start ssh-agent-switcher");

        if !wait_for_path(&switcher_socket, Duration::from_secs(5)) {
            send_signal(agent_pid as libc::pid_t, libc::SIGTERM);
            panic!("Switcher socket was not created");
        }

        Some(Self {
            _temp_dir: temp_dir,
            _agent_pid: agent_pid,
            _switcher: SwitcherProcess {
                pid: child.id() as libc::pid_t,
                socket_path: switcher_socket.clone(),
            },
            switcher_socket,
            sockets_root,
            log_file,
        })
    }
}

impl Drop for Openssh101Env {
    fn drop(&mut self) {
        send_signal(self._agent_pid as libc::pid_t, libc::SIGTERM);
    }
}

#[test]
fn test_openssh_10_1_list_identities() {
    let Some(env) = Openssh101Env::new() else {
        eprintln!("Skipping test: ssh-agent not available");
        return;
    };

    let output = Command::new("ssh-add")
        .arg("-l")
        .env("SSH_AUTH_SOCK", &env.switcher_socket)
        .output()
        .expect("Failed to run ssh-add");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);

    assert!(
        combined.to_lowercase().contains("no identities"),
        "Expected 'no identities', got: {}",
        combined
    );
}

#[test]
fn test_openssh_10_1_add_identity() {
    let Some(env) = Openssh101Env::new() else {
        eprintln!("Skipping test: ssh-agent not available");
        return;
    };

    let key_file = env._temp_dir.path().join("id_rsa");

    // Generate a test key
    let status = Command::new("ssh-keygen")
        .args(["-t", "rsa", "-b", "1024", "-N", "", "-f"])
        .arg(&key_file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("Failed to run ssh-keygen");

    assert!(status.success(), "ssh-keygen failed");

    // Add the key
    let output = Command::new("ssh-add")
        .arg(&key_file)
        .env("SSH_AUTH_SOCK", &env.switcher_socket)
        .output()
        .expect("Failed to run ssh-add");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Identity added"),
        "Expected 'Identity added', got: {}",
        stderr
    );
}

#[test]
fn test_openssh_10_1_ignore_unknown_files() {
    let Some(env) = Openssh101Env::new() else {
        eprintln!("Skipping test: ssh-agent not available");
        return;
    };

    // Create garbage in the sockets directory
    fs::write(env.sockets_root.join("file-unknown"), "").unwrap();
    fs::create_dir(env.sockets_root.join("dir-unknown")).unwrap();
    fs::write(env.sockets_root.join("agent.not-a-socket"), "").unwrap();
    fs::write(env.sockets_root.join("not-a-socket.sshd.foobar"), "").unwrap();

    // Run ssh-add to trigger socket discovery
    let output = Command::new("ssh-add")
        .arg("-l")
        .env("SSH_AUTH_SOCK", &env.switcher_socket)
        .output()
        .expect("Failed to run ssh-add");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.to_lowercase().contains("no identities"),
        "Expected 'no identities', got: {}",
        combined
    );

    // Check log for correct ignore messages
    let log = fs::read_to_string(&env.log_file).unwrap_or_default();

    assert!(
        log.contains("file-unknown") && log.contains("agent"),
        "Expected ignore message for file-unknown"
    );
    assert!(
        log.contains("dir-unknown") && log.contains("agent"),
        "Expected ignore message for dir-unknown"
    );
    assert!(
        log.contains("agent.not-a-socket") && log.contains("Cannot connect"),
        "Expected ignore message for agent.not-a-socket"
    );
    assert!(
        log.contains("not-a-socket.sshd.foobar") && log.contains("Cannot connect"),
        "Expected ignore message for not-a-socket.sshd.foobar"
    );
}

// =============================================================================
// daemonize fixture tests
// =============================================================================

#[test]
fn test_daemonize_xdg_dirs() {
    let temp_dir = TempDir::new().unwrap();
    let fake_home = temp_dir.path().join("home");
    fs::create_dir(&fake_home).unwrap();

    let state_dir = temp_dir.path().join("state");
    let runtime_dir = temp_dir.path().join("runtime");
    fs::create_dir(&runtime_dir).unwrap();
    fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700)).unwrap();

    let sockets_root = temp_dir.path().join("sockets");
    fs::create_dir(&sockets_root).unwrap();

    let socket = sockets_root.join("socket");
    let expected_log = state_dir.join("ssh-agent-switcher.log");
    let expected_pid = runtime_dir.join("ssh-agent-switcher.pid");

    let status = Command::new(binary_path())
        .arg("--daemon")
        .arg("--socket-path")
        .arg(&socket)
        .env("HOME", &fake_home)
        .env("XDG_STATE_HOME", &state_dir)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .status()
        .expect("Failed to start ssh-agent-switcher");

    assert!(status.success(), "Daemon parent should exit successfully");
    assert!(expected_pid.exists(), "PID file should be created at XDG location");
    assert!(expected_log.exists(), "Log file should be created at XDG location");

    // Read PID and kill daemon
    let pid: libc::pid_t = fs::read_to_string(&expected_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    send_signal(pid, libc::SIGTERM);
    assert!(
        wait_for_path_gone(&expected_pid, Duration::from_secs(2)),
        "PID file should be removed on shutdown"
    );
}

#[test]
fn test_daemonize_xdg_runtime_dir_not_set() {
    let temp_dir = TempDir::new().unwrap();
    let fake_home = temp_dir.path().join("home");
    fs::create_dir(&fake_home).unwrap();

    let state_dir = temp_dir.path().join("state");
    fs::create_dir(&state_dir).unwrap();
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();

    let sockets_root = temp_dir.path().join("sockets");
    fs::create_dir(&sockets_root).unwrap();

    let socket = sockets_root.join("socket");
    let expected_log = state_dir.join("ssh-agent-switcher.log");
    // When XDG_RUNTIME_DIR is not set, PID file falls back to state dir
    let expected_pid = state_dir.join("ssh-agent-switcher.pid");

    let status = Command::new(binary_path())
        .arg("--daemon")
        .arg("--socket-path")
        .arg(&socket)
        .env("HOME", &fake_home)
        .env("XDG_STATE_HOME", &state_dir)
        .env_remove("XDG_RUNTIME_DIR")
        .status()
        .expect("Failed to start ssh-agent-switcher");

    assert!(status.success(), "Daemon parent should exit successfully");
    assert!(
        expected_pid.exists(),
        "PID file should fall back to state dir when XDG_RUNTIME_DIR not set"
    );
    assert!(expected_log.exists(), "Log file should be created");

    // Read PID and kill daemon
    let pid: libc::pid_t = fs::read_to_string(&expected_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    send_signal(pid, libc::SIGTERM);
    wait_for_path_gone(&expected_pid, Duration::from_secs(2));
}

#[test]
fn test_daemonize_explicit_files() {
    let temp_dir = TempDir::new().unwrap();
    let fake_home = temp_dir.path().join("home");
    fs::create_dir(&fake_home).unwrap();

    let sockets_root = temp_dir.path().join("sockets");
    fs::create_dir(&sockets_root).unwrap();

    let socket = sockets_root.join("socket");
    let log_file = temp_dir.path().join("explicit.log");
    let pid_file = temp_dir.path().join("explicit.pid");

    let status = Command::new(binary_path())
        .arg("--daemon")
        .arg("--socket-path")
        .arg(&socket)
        .arg("--log-file")
        .arg(&log_file)
        .arg("--pid-file")
        .arg(&pid_file)
        .env("HOME", &fake_home)
        .status()
        .expect("Failed to start ssh-agent-switcher");

    assert!(status.success(), "Daemon parent should exit successfully");
    assert!(pid_file.exists(), "Explicit PID file should be created");
    assert!(log_file.exists(), "Explicit log file should be created");

    // Read PID and kill daemon
    let pid: libc::pid_t = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    send_signal(pid, libc::SIGTERM);
    assert!(
        wait_for_path_gone(&pid_file, Duration::from_secs(2)),
        "PID file should be removed on shutdown"
    );
}

#[test]
fn test_daemonize_double_start() {
    let temp_dir = TempDir::new().unwrap();
    let fake_home = temp_dir.path().join("home");
    fs::create_dir(&fake_home).unwrap();

    let sockets_root = temp_dir.path().join("sockets");
    fs::create_dir(&sockets_root).unwrap();

    let socket = sockets_root.join("socket");
    let socket2 = sockets_root.join("socket2");
    let log_file = temp_dir.path().join("test.log");
    let pid_file = temp_dir.path().join("test.pid");

    // Start first daemon
    let status = Command::new(binary_path())
        .arg("--daemon")
        .arg("--socket-path")
        .arg(&socket)
        .arg("--log-file")
        .arg(&log_file)
        .arg("--pid-file")
        .arg(&pid_file)
        .env("HOME", &fake_home)
        .status()
        .expect("Failed to start ssh-agent-switcher");

    assert!(status.success(), "First daemon should start successfully");

    // Try to start second daemon with same PID file but different socket
    // The child will detect the lock and exit, but the parent may timeout waiting
    // for socket2 (which will never be created). We don't care about the exit status,
    // only that socket2 is never created.
    let _ = Command::new(binary_path())
        .arg("--daemon")
        .arg("--socket-path")
        .arg(&socket2)
        .arg("--log-file")
        .arg(&log_file)
        .arg("--pid-file")
        .arg(&pid_file)
        .env("HOME", &fake_home)
        .status();

    // Verify second socket was NOT created (the main point of this test)
    assert!(
        !socket2.exists(),
        "Second daemon should not have started - socket2 should not exist"
    );

    // Clean up first daemon
    let pid: libc::pid_t = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    send_signal(pid, libc::SIGTERM);
    wait_for_path_gone(&pid_file, Duration::from_secs(2));
}
