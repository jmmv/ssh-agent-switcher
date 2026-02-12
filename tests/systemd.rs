//! Tests for systemd socket activation.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tempfile::TempDir;

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

/// Spawn a backend echo server on a Unix socket.
fn spawn_echo_backend(socket_path: &std::path::Path) -> BackendTask {
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
                    thread::spawn(move || {
                        let mut stream = stream;
                        let mut buf = [0u8; 1024];
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    if stream.write_all(&buf[..n]).is_err() {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    });
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

impl Drop for BackendTask {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Connect to wake up the blocking accept()
        let _ = UnixStream::connect(&self.socket_path);
    }
}

/// Test systemd socket activation.
///
/// This test simulates systemd socket activation by:
/// 1. Creating a UnixListener bound to a socket path
/// 2. Using pre_exec to set up fd 3 and LISTEN_PID (using getpid() after fork)
/// 3. Verifying the binary uses the pre-bound socket
#[test]
fn test_systemd_activation() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let socket_path = temp_dir.path().join("systemd.sock");

    // Create agent directory structure (must be ssh-* subdirectory with agent.* socket)
    let agents_dir = temp_dir.path();
    let backend_dir = agents_dir.join("ssh-backend");
    fs::create_dir(&backend_dir).expect("Failed to create backend dir");

    // Create backend socket so switcher has something to forward to
    let backend = spawn_echo_backend(&backend_dir.join("agent.test"));

    // Create the listener that simulates systemd's socket
    let listener = UnixListener::bind(&socket_path).expect("Failed to bind socket");
    let fd = listener.as_raw_fd();

    let mut cmd = Command::new(binary_path());
    cmd.arg("--agents-dirs")
        .arg(agents_dir)
        .env("RUST_LOG", "info")
        .env("LISTEN_FDS", "1")
        // Don't set LISTEN_PID - listenfd will skip the PID check if it's empty
        .env_remove("LISTEN_PID");

    // Use pre_exec to set up fd 3 (where systemd places sockets)
    unsafe {
        cmd.pre_exec(move || {
            // Systemd convention: fds start at 3
            if fd != 3 {
                if libc::dup2(fd, 3) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(fd);
            }
            // Clear the close-on-exec flag so the fd is inherited
            let flags = libc::fcntl(3, libc::F_GETFD);
            if flags == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().expect("Failed to spawn");

    // Drop our copy of the listener so the fd is only held by the child
    drop(listener);

    // Give it time to start
    thread::sleep(Duration::from_millis(200));

    // Connect to the socket and verify it works
    let mut client = UnixStream::connect(&socket_path).expect("Failed to connect to socket");
    client.set_read_timeout(Some(Duration::from_secs(2))).expect("Failed to set timeout");
    client.write_all(b"hello").expect("Failed to write");

    let mut response = vec![0u8; 5];
    client.read_exact(&mut response).expect("Failed to read");
    assert_eq!(response, b"hello", "Should echo back the message");

    // Clean up
    drop(backend);
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    child.wait().expect("Failed to wait for child");

    // With systemd activation, the socket should NOT be removed (systemd owns it)
    assert!(
        socket_path.exists(),
        "Socket should not be removed in systemd activation mode"
    );
}
