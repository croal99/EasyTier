//! Cross-platform PTY (pseudo-terminal) support via the `xpty` crate.
//!
//! `xpty` wraps both Windows ConPTY and Unix openpty under a unified interface.
//! Since `xpty` returns sync `Read`/`Write` handles, we bridge them to Tokio's
//! async world via dedicated OS threads + mpsc channels.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
// use crate::common::log::info;

// ── Public types ──────────────────────────────────────────────────────────

/// Holds the spawned PTY master (for resize) and child process (for cleanup).
pub struct ShellProcess {
    inner: Option<PtyInner>,
}

/// Async I/O streams for the spawned shell process.
pub struct ShellStreams {
    pub stdin: Box<dyn AsyncWrite + Unpin + Send>,
    pub stdout: Box<dyn AsyncRead + Unpin + Send>,
}

// ── spawn_shell ───────────────────────────────────────────────────────────

/// Spawn a system shell inside a PTY with the given terminal dimensions.
pub fn spawn_shell(cols: u16, rows: u16) -> io::Result<(ShellProcess, ShellStreams)> {
    use xpty::{PtySize, PtySystem};

    let pty_system = xpty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(map_err)?;

    // tracing::info!(cols, rows, "xpty PTY opened");

    let cmd = make_shell_command();
    let child = pair.slave.spawn_command(cmd).map_err(map_err)?;

    let reader = pair.master.try_clone_reader().map_err(map_err)?;
    let writer = pair.master.take_writer().map_err(map_err)?;

    // Bridge sync I/O to tokio async world.
    let stdout = spawn_reader_thread(reader);
    let stdin = spawn_writer_thread(writer);

    let inner = PtyInner {
        master: pair.master,
        child,
    };

    let proc = ShellProcess {
        inner: Some(inner),
    };
    let streams = ShellStreams { stdin, stdout };

    Ok((proc, streams))
}

// ── ShellProcess methods ──────────────────────────────────────────────────

impl ShellProcess {
    /// Resize the terminal window. Works on all platforms via xpty.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        if let Some(ref inner) = self.inner {
            inner.resize(cols, rows)
        } else {
            Ok(())
        }
    }
}

// ── Internal: PtyInner ────────────────────────────────────────────────────

struct PtyInner {
    master: Box<dyn xpty::MasterPty>,
    #[allow(dead_code)]
    child: Box<dyn xpty::Child + Send + Sync>,
}

// `MasterPty` trait object does not auto-derive Send/Sync (the trait
// does not inherit them).  We own it exclusively and only call `resize()`
// which the xpty crate handles thread-safely via internal synchronization.
// `child` is already `Send + Sync` from `spawn_command`.
unsafe impl Send for PtyInner {}
unsafe impl Sync for PtyInner {}

impl PtyInner {
    fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        self.master
            .resize(xpty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(map_err)
    }
}

// ── Helper: build the shell command ───────────────────────────────────────

fn make_shell_command() -> xpty::CommandBuilder {
    #[cfg(windows)]
    {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| {
            format!(
                "{}\\System32\\cmd.exe",
                std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())
            )
        });
        xpty::CommandBuilder::new(shell)
    }

    #[cfg(not(windows))]
    {
        let shell = if std::path::Path::new("/bin/bash").exists() {
            "/bin/bash"
        } else {
            "/bin/sh"
        };
        let mut cmd = xpty::CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("HOME", &std::env::var("HOME").unwrap_or_else(|_| "/root".into()));
        cmd.env("LANG", "C.UTF-8");
        cmd.env("LC_ALL", "C.UTF-8");
        cmd.env("HISTFILE", "/dev/null"); // Disable shell history
        cmd
    }
}

/// Spawn a command via the system shell and return combined stdout+stderr.
///
/// Uses plain `std::process::Command` (matching Go's `os/exec`) — no PTY is
/// needed for one-off `exec` requests.  The blocking call runs on
/// `spawn_blocking` to keep the async runtime responsive.
pub async fn spawn_exec(command: &str) -> io::Result<String> {
    #[cfg(windows)]
    let (shell, shell_arg) = {
        let s = std::env::var("COMSPEC")
            .unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".into());
        (s, "/c")
    };
    
    #[cfg(not(windows))]
    let (shell, shell_arg) = ("/bin/sh".to_string(), "-c");

    #[cfg(windows)]
    // 在 Windows 上，执行命令前先通过 chcp 65001 将控制台代码页切换到 UTF-8
    let cmd = format!("chcp 65001 >nul && {}", command);

    #[cfg(not(windows))]
    let cmd = command.to_string();

    let output = tokio::task::spawn_blocking({
        let cmd = cmd.clone();
        move || {
            std::process::Command::new(&shell)
                .arg(shell_arg)
                .arg(&cmd)
                .env("LANG", "C.UTF-8")
                .env("LC_ALL", "C.UTF-8")
                .output()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
        }
    })
    .await
    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))??;

    let mut text = Vec::with_capacity(output.stdout.len() + output.stderr.len());
    text.extend_from_slice(&output.stdout);
    text.extend_from_slice(&output.stderr);

    let text = String::from_utf8_lossy(&text).into_owned();
    // info!("Output text: {}", text);
    Ok(text)
}

// ── Sync → Async bridges ──────────────────────────────────────────────────

/// Spawn a dedicated OS thread that reads from the PTY synchronously
/// and forwards chunks through an mpsc channel into the tokio runtime.
fn spawn_reader_thread(
    mut reader: Box<dyn std::io::Read + Send>,
) -> Box<dyn AsyncRead + Unpin + Send> {
    use std::io::Read;

    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 4096];
        let mut total: u64 = 0;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    tracing::debug!(total, "xpty reader: EOF");
                    break;
                }
                Ok(n) => {
                    total += n as u64;
                    tracing::trace!(n, total, "xpty reader: data");
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!(?e, total, "xpty reader: read error");
                    break;
                }
            }
        }
    });

    Box::new(PtyReader { rx, pending: None })
}

/// Spawn a dedicated OS thread that receives chunks from an mpsc channel
/// and writes them synchronously into the PTY.
fn spawn_writer_thread(
    mut writer: Box<dyn std::io::Write + Send>,
) -> Box<dyn AsyncWrite + Unpin + Send> {
    use std::io::Write;

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut total: u64 = 0;
        while let Some(data) = rx.blocking_recv() {
            total += data.len() as u64;
            if let Err(e) = writer.write_all(&data) {
                tracing::error!(?e, total, "xpty writer: write error");
                break;
            }
            let _ = writer.flush();
        }
        // tracing::debug!(total, "xpty writer: channel closed");
    });

    Box::new(PtyWriter { tx })
}

// ── Async adapters ────────────────────────────────────────────────────────

struct PtyReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    pending: Option<(Vec<u8>, usize)>,
}

impl AsyncRead for PtyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            // Serve cached data first.
            if let Some((ref data, ref mut offset)) = self.pending {
                let remaining = data.len() - *offset;
                if remaining > 0 {
                    let n = remaining.min(buf.remaining());
                    buf.put_slice(&data[*offset..*offset + n]);
                    *offset += n;
                    if *offset >= data.len() {
                        self.pending = None;
                    }
                    return Poll::Ready(Ok(()));
                }
                // Fully consumed – clear stale entry.
                self.pending = None;
            }

            // Pull next chunk from the reader thread.
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(data)) => {
                    self.pending = Some((data, 0));
                    // Loop back to serve it immediately.
                }
                Poll::Ready(None) => {
                    // Channel closed → EOF.
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

struct PtyWriter {
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl AsyncWrite for PtyWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let len = buf.len();
        self.tx
            .send(buf.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "xpty write closed"))?;
        Poll::Ready(Ok(len))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn map_err(e: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}
