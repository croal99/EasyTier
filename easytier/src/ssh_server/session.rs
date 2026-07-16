use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::management_cli::EmbeddedCommandRouter;
use tokio::io::AsyncWriteExt;

use super::pty;

#[derive(Clone)]
pub struct ServerHandle {
    router: Arc<EmbeddedCommandRouter>,
}

impl ServerHandle {
    /// Create a new SSH server handle that can spawn per-connection handlers.
    pub fn new(router: Arc<EmbeddedCommandRouter>) -> Self {
        Self { router }
    }
}

enum EscState {
    /// Received 0x1B, waiting for the next byte to determine sequence type.
    ExpectType,
    /// ESC [ CSI – accumulating parameters and awaiting final byte (0x40..0x7E).
    Csi,
    /// ESC O SS3 – awaiting final byte (0x20..0x7E).
    Ss3,
    /// Some other single-character ESC sequence – consuming one more byte.
    EscOther,
}

enum SessionMode {
    /// Forwarding bytes between SSH client and a spawned system shell.
    SystemShell,
    /// Bluenet embedded command-line interface.
    Bluenet,
}

pub struct SessionHandle {
    router: Arc<EmbeddedCommandRouter>,
    // ── Shared state ────────────────────────────────────────────
    channel: Option<russh::ChannelId>,
    /// The active interaction mode.
    mode: SessionMode,
    // ── Shell-mode state ────────────────────────────────────────
    /// Stdin handle of the spawned system shell (set when a shell is active).
    shell_stdin: Option<Box<dyn tokio::io::AsyncWrite + Unpin + Send>>,
    /// Line buffer used to detect `@bluenet` trigger while in shell mode.
    shell_line_buf: Vec<u8>,
    /// Set to true by the background reader task when the shell process exits.
    shell_exited: Arc<AtomicBool>,
    /// PTY process handle for resize and cleanup.
    pty_proc: Option<pty::ShellProcess>,
    // ── Bluenet-mode state ──────────────────────────────────────
    /// Input line buffer for bluenet commands.
    buf: Vec<u8>,
    last_was_cr: bool,
    /// Command history, most recent command last.
    history: Vec<String>,
    /// Current position in history navigation; None means at the "fresh" line.
    history_index: Option<usize>,
    /// Saved input line before entering history navigation.
    saved_buf: Vec<u8>,
    /// ESC-sequence parser state.  None = normal mode.
    esc_state: Option<EscState>,
    /// Whether a PTY has been allocated for the current session.
    pty_allocated: bool,
    /// Whether PTY ECHO mode is enabled (ECHO=1 in pty modes).
    /// When true, the server is responsible for echoing user input.
    pty_echo: bool,
}

impl SessionHandle {
    /// Maximum number of history entries to retain.
    const MAX_HISTORY: usize = 500;

    /// Create a per-connection session handler.
    fn new(router: Arc<EmbeddedCommandRouter>) -> Self {
        Self {
            router,
            channel: None,
            mode: SessionMode::SystemShell,
            shell_stdin: None,
            shell_line_buf: Vec::new(),
            shell_exited: Arc::new(AtomicBool::new(false)),
            pty_proc: None,
            buf: Vec::new(),
            last_was_cr: false,
            history: Vec::new(),
            history_index: None,
            saved_buf: Vec::new(),
            esc_state: None,
            pty_allocated: false,
            pty_echo: false,
        }
    }

    // ── Bluenet-mode helpers ─────────────────────────────────────

    /// Redraw the current input line.  Used after navigating history.
    fn redraw_line(
        &mut self,
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
    ) -> Result<(), russh::Error> {
        // Erase the entire line visually and go back to start.
        Self::write_raw(session, channel, "\r\x1b[K")?;
        Self::write_raw(session, channel, Self::prompt())?;
        let text = String::from_utf8_lossy(&self.buf);
        if !text.is_empty() {
            Self::write_raw(session, channel, text.as_ref())?;
        }
        Ok(())
    }

    /// Navigate to the previous history entry (up arrow).
    fn history_prev(
        &mut self,
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
    ) -> Result<(), russh::Error> {
        if self.history.is_empty() {
            return Ok(());
        }
        match self.history_index {
            None => {
                // Save current line for "back to future" navigation.
                self.saved_buf = self.buf.clone();
                let idx = self.history.len() - 1;
                self.history_index = Some(idx);
                self.buf = self.history[idx].as_bytes().to_vec();
            }
            Some(0) => {
                // Already at oldest entry; stay there.
                return Ok(());
            }
            Some(idx) => {
                let idx = idx - 1;
                self.history_index = Some(idx);
                self.buf = self.history[idx].as_bytes().to_vec();
            }
        }
        self.redraw_line(session, channel)
    }

    /// Navigate to the next history entry (down arrow).
    fn history_next(
        &mut self,
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
    ) -> Result<(), russh::Error> {
        match self.history_index {
            None => return Ok(()),
            Some(idx) if idx + 1 >= self.history.len() => {
                // Past the newest entry – restore saved "fresh" buffer.
                self.history_index = None;
                self.buf = self.saved_buf.clone();
                self.saved_buf.clear();
            }
            Some(idx) => {
                let idx = idx + 1;
                self.history_index = Some(idx);
                self.buf = self.history[idx].as_bytes().to_vec();
            }
        }
        self.redraw_line(session, channel)
    }

    fn prompt() -> &'static str {
        "\x1b[34mbluenet> \x1b[0m"
    }

    fn write_prompt(
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
    ) -> Result<(), russh::Error> {
        session.data(channel, russh::CryptoVec::from(Self::prompt()))?;
        Ok(())
    }

    fn write_text(
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
        text: String,
    ) -> Result<(), russh::Error> {
        if !text.is_empty() {
            // Normalize line endings: PTY requires \r\n for proper display.
            // Bare \n moves cursor down without returning to column 0, causing a staircase effect.
            let text = text.replace("\r\n", "\n").replace('\n', "\r\n");
            session.data(channel, russh::CryptoVec::from(text))?;
            session.data(channel, russh::CryptoVec::from("\r\n"))?;
        }
        Ok(())
    }

    /// Write a raw control sequence or text fragment back to the SSH client.
    fn write_raw(
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
        text: &str,
    ) -> Result<(), russh::Error> {
        session.data(channel, russh::CryptoVec::from(text))?;
        Ok(())
    }

    /// Write raw bytes back to the SSH client (no UTF-8 validation).
    fn write_raw_bytes(
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
        data: &[u8],
    ) -> Result<(), russh::Error> {
        session.data(channel, russh::CryptoVec::from(data))?;
        Ok(())
    }

    /// Push a successfully executed non-empty command line into history.
    fn push_history(&mut self, line: String) {
        // Avoid consecutive duplicates.
        if self.history.last().map(|s| s.as_str()) == Some(line.as_str()) {
            return;
        }
        self.history.push(line);
        if self.history.len() > Self::MAX_HISTORY {
            self.history.remove(0);
        }
        // Reset navigation state since we just executed a new command.
        self.history_index = None;
        self.saved_buf.clear();
    }

    /// Execute one completed bluenet command line and print the result.
    /// Returns `true` if the session should close entirely.
    async fn execute_bluenet_line(
        &mut self,
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
        line: String,
    ) -> Result<bool, russh::Error> {
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            Self::write_prompt(session, channel)?;
            return Ok(false);
        }

        self.push_history(trimmed.clone());

        let res = self.router.execute_line(&trimmed).await;
        match res {
            Ok(r) => {
                Self::write_text(session, channel, r.output)?;
                if r.should_exit {
                    // Instead of closing the SSH session, return to system shell.
                    self.enter_shell_mode(session, channel).await?;
                    return Ok(false);
                }
            }
            Err(e) => {
                Self::write_text(session, channel, format!("ERROR: {e}"))?;
            }
        }

        Self::write_prompt(session, channel)?;
        Ok(false)
    }

    // ── Mode transitions ──────────────────────────────────────────

    /// Transition from any mode into Bluenet mode.
    async fn enter_bluenet_mode(
        &mut self,
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
    ) -> Result<(), russh::Error> {
        self.mode = SessionMode::Bluenet;
        self.buf.clear();
        self.esc_state = None;
        self.saved_buf.clear();
        // Print transition banner and prompt.
        Self::write_raw(session, channel, "\r\n\x1b[36m=== Bluenet embedded CLI ===\x1b[0m\r\n")?;
        Self::write_raw(
            session,
            channel,
            "\x1b[33mType 'help' for commands, 'exit' to return to shell.\x1b[0m\r\n",
        )?;
        Self::write_prompt(session, channel)?;
        Ok(())
    }

    /// Transition from Bluenet mode back into system-shell mode.
    async fn enter_shell_mode(
        &mut self,
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
    ) -> Result<(), russh::Error> {
        self.mode = SessionMode::SystemShell;
        self.shell_line_buf.clear();
        // Print transition message.
        Self::write_raw(session, channel, "\r\n\x1b[36m=== System shell ===\x1b[0m\r\n")?;
        Self::write_raw(
            session,
            channel,
            "\x1b[33mType @bluenet to enter embedded CLI.\x1b[0m\r\n",
        )?;

        // Send a newline to the shell to force a fresh prompt.
        if let Some(ref mut stdin) = self.shell_stdin {
            let _ = stdin.write_all(b"\n").await;
            let _ = stdin.flush().await;
        }
        Ok(())
    }

    /// Shut down the SSH session cleanly.
    fn close_session(
        &mut self,
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
    ) -> Result<(), russh::Error> {
        session.exit_status_request(channel, 0)?;
        session.close(channel)?;
        self.channel = None;
        Ok(())
    }

    // ── Shell-mode data handling ──────────────────────────────────

    /// Forward input bytes to the system shell, buffering to detect `@bluenet` trigger.
    /// Returns `true` if the session has closed.
    async fn handle_shell_data(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<bool, russh::Error> {
        if self.shell_exited.load(Ordering::SeqCst) {
            self.close_session(session, channel)?;
            return Ok(true);
        }

        let stdin = match self.shell_stdin.as_mut() {
            Some(s) => s,
            None => {
                self.close_session(session, channel)?;
                return Ok(true);
            }
        };

        for &byte in data.iter() {
            match byte {
                b'\r' | b'\n' => {
                    // End of line – check for @bluenet trigger.
                    let line = String::from_utf8_lossy(&self.shell_line_buf);
                    if line.trim() == "@bluenet" {
                        self.shell_line_buf.clear();
                        // Send Ctrl+C to cancel the shell prompt, then enter bluenet mode.
                        let _ = stdin.write_all(b"\x03").await;
                        self.enter_bluenet_mode(session, channel).await?;
                        return Ok(false);
                    }
                    // Not a trigger – forward the newline and clear buffer.
                    self.shell_line_buf.clear();
                    stdin.write_all(&[byte]).await.ok();
                }
                0x08 | 0x7F => {
                    // Backspace: pop from line buffer.
                    self.shell_line_buf.pop();
                    stdin.write_all(&[byte]).await.ok();
                }
                b => {
                    // Track all non-control bytes including multi-byte UTF-8.
                    if b >= 0x20 {
                        self.shell_line_buf.push(b);
                    }
                    stdin.write_all(&[b]).await.ok();
                }
            }
        }

        // Flush so the shell receives input promptly.
        let _ = stdin.flush().await;
        Ok(false)
    }

    // ── Bluenet-mode data handling ────────────────────────────────

    /// Process a single chunk of SSH client data while in Bluenet mode.
    /// Returns `true` if the session has closed.
    async fn handle_bluenet_data(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<bool, russh::Error> {
        // When a PTY is allocated with ECHO=0 the client handles local echo.
        let should_echo = !self.pty_allocated || self.pty_echo;

        for &byte in data.iter() {
            // ── ESC-sequence parser ────────────────────────────────
            match self.esc_state.take() {
                Some(EscState::ExpectType) => {
                    match byte {
                        b'[' => {
                            self.esc_state = Some(EscState::Csi);
                        }
                        b'O' => {
                            self.esc_state = Some(EscState::Ss3);
                        }
                        _ => {
                            // Single-byte ESC sequence (e.g. ESC =, ESC >).
                            // Already consumed the second byte – discard.
                        }
                    }
                    continue;
                }
                Some(EscState::Csi) => {
                    // CSI: ESC [ (parameter bytes 0x30-0x3F) (intermediate bytes 0x20-0x2F) <final 0x40-0x7E>
                    match byte {
                        0x30..=0x3F | 0x20..=0x2F => {
                            // Still inside parameters or intermediates.
                            self.esc_state = Some(EscState::Csi);
                        }
                        0x40..=0x7E => {
                            // Final byte – interpret known sequences.
                            match byte {
                                b'A' => self.history_prev(session, channel)?,
                                b'B' => self.history_next(session, channel)?,
                                _ => { /* C=right, D=left, H=home, F=end, etc. – ignored */ }
                            }
                        }
                        _ => {
                            // Unrecognized byte; abort the sequence.
                        }
                    }
                    continue;
                }
                Some(EscState::Ss3) => {
                    // SS3: ESC O <final byte 0x20-0x7E>
                    // (often used for F1-F4 keys)
                    // Consume and ignore.
                    continue;
                }
                Some(EscState::EscOther) => {
                    // Two-byte ESC sequence second byte – consumed.
                    continue;
                }
                None => {}
            }

            // ── Printable / control bytes ──────────────────────────
            match byte {
                b'\r' => {
                    self.last_was_cr = true;
                    Self::write_raw(session, channel, "\r\n")?;
                    let line = String::from_utf8_lossy(&self.buf).to_string();
                    self.buf.clear();
                    if self.execute_bluenet_line(session, channel, line).await? {
                        return Ok(true);
                    }
                }
                b'\n' => {
                    if self.last_was_cr {
                        self.last_was_cr = false;
                        continue;
                    }

                    Self::write_raw(session, channel, "\r\n")?;
                    let line = String::from_utf8_lossy(&self.buf).to_string();
                    self.buf.clear();
                    if self.execute_bluenet_line(session, channel, line).await? {
                        return Ok(true);
                    }
                }
                0x08 | 0x7f => {
                    self.last_was_cr = false;
                    if let Some(removed) = self.buf.pop() {
                        // For multi-byte UTF-8 characters, also strip continuation bytes.
                        if removed >= 0x80 {
                            while self
                                .buf
                                .last()
                                .map_or(false, |&b| (0x80..=0xBF).contains(&b))
                            {
                                self.buf.pop();
                            }
                        }
                        if should_echo {
                            Self::write_raw(session, channel, "\u{8} \u{8}")?;
                        }
                    }
                }
                0x1b => {
                    // Start of an ANSI/VT100 escape sequence.
                    self.esc_state = Some(EscState::ExpectType);
                }
                0x03 => {
                    // Ctrl+C: discard current input line.
                    self.last_was_cr = false;
                    self.buf.clear();
                    self.esc_state = None;
                    self.saved_buf.clear();
                    Self::write_raw(session, channel, "^C\r\n")?;
                    Self::write_prompt(session, channel)?;
                }
                0x04 => {
                    // Ctrl+D = return to system shell if line is empty, else discard.
                    self.last_was_cr = false;
                    if self.buf.is_empty() {
                        Self::write_raw(session, channel, "\r\n")?;
                        self.enter_shell_mode(session, channel).await?;
                    }
                }
                byte => {
                    self.last_was_cr = false;
                    // Accept printable ASCII and multi-byte UTF-8 lead/continuation bytes.
                    if byte >= 0x20 {
                        self.buf.push(byte);
                        if should_echo {
                            // Echo the raw byte – required for multi-byte UTF-8 sequences.
                            Self::write_raw_bytes(session, channel, &[byte])?;
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    // ── Background shell reader task ──────────────────────────────

    /// Reads from the shell PTY/piped output and writes to the SSH channel.
    /// Sets `shell_exited` when the child process terminates (EOF on stdout).
    async fn shell_reader_task(
        mut output: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
        handle: russh::server::Handle,
        channel: russh::ChannelId,
        shell_exited: Arc<AtomicBool>,
    ) {
        use tokio::io::AsyncReadExt;

        tracing::info!("shell reader task started");
        let mut out_buf = [0u8; 4096];
        let mut total_bytes: u64 = 0;
        loop {
            match output.read(&mut out_buf).await {
                Ok(0) => {
                    tracing::info!(total_bytes, "shell reader: EOF");
                    break;
                }
                Ok(n) => {
                    total_bytes += n as u64;
                    tracing::trace!(n, total_bytes, "shell reader: forwarding to SSH");
                    if handle
                        .data(channel, russh::CryptoVec::from(&out_buf[..n]))
                        .await
                        .is_err()
                    {
                        tracing::warn!(total_bytes, "shell reader: SSH channel closed");
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!(?e, total_bytes, "shell reader: read error");
                    break;
                }
            }
        }

        tracing::info!(total_bytes, "shell reader task exiting");
        shell_exited.store(true, Ordering::SeqCst);
    }
}

impl russh::server::Server for ServerHandle {
    type Handler = SessionHandle;

    fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> SessionHandle {
        SessionHandle::new(self.router.clone())
    }
}

impl russh::server::Handler for SessionHandle {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        public_key: &russh::keys::PublicKey,
    ) -> Result<russh::server::Auth, Self::Error> {
        if super::auth::is_authorized_public_key(public_key) {
            Ok(russh::server::Auth::Accept)
        } else {
            Ok(russh::server::Auth::Reject {
                proceed_with_methods: None,
            })
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: russh::Channel<russh::server::Msg>,
        _session: &mut russh::server::Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn env_request(
        &mut self,
        channel: russh::ChannelId,
        _name: &str,
        _value: &str,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: russh::ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        modes: &[(russh::Pty, u32)],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.pty_allocated = true;

        // Extract the ECHO flag from PTY modes.
        // ECHO=1 means the server should echo; ECHO=0 means the client does local echo.
        self.pty_echo = modes
            .iter()
            .any(|&(code, val)| code == russh::Pty::ECHO && val != 0);

        session.channel_success(channel)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: russh::ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if let Some(ref pty) = self.pty_proc {
            let _ = pty.resize(col_width as u16, row_height as u16);
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: russh::ChannelId,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.channel = Some(channel);

        // Spawn the system shell via platform PTY (ConPTY on Windows, pipes on Unix).
        let (pty_proc, streams) = pty::spawn_shell(80, 24).map_err(russh::Error::from)?;

        self.shell_stdin = Some(streams.stdin);
        self.shell_line_buf.clear();
        self.shell_exited = Arc::new(AtomicBool::new(false));
        self.pty_proc = Some(pty_proc);
        self.mode = SessionMode::SystemShell;

        // Start a background task that reads shell output and writes to SSH channel.
        let handle = session.handle().clone();
        let shell_exited = self.shell_exited.clone();

        tokio::spawn(async move {
            SessionHandle::shell_reader_task(
                streams.stdout,
                handle,
                channel,
                shell_exited,
            )
            .await;
        });

        session.channel_success(channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let line = String::from_utf8_lossy(data).trim().to_string();
        tracing::info!(command = %line, "SSH exec request");

        let output = pty::spawn_exec(&line).await;

        match output {
            Ok(text) => {
                session.channel_success(channel)?;
                if !text.is_empty() {
                    Self::write_text(session, channel, text)?;
                }
                session.exit_status_request(channel, 0)?;
            }
            Err(e) => {
                tracing::error!(command = %line, ?e, "exec failed");
                session.channel_failure(channel)?;
                return Ok(());
            }
        }

        session.close(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if self.channel != Some(channel) {
            return Ok(());
        }

        if self.shell_exited.load(Ordering::SeqCst) {
            self.close_session(session, channel)?;
            return Ok(());
        }

        match self.mode {
            SessionMode::SystemShell => {
                if self.handle_shell_data(channel, data, session).await? {
                    return Ok(());
                }
            }
            SessionMode::Bluenet => {
                if self.handle_bluenet_data(channel, data, session).await? {
                    return Ok(());
                }
            }
        }

        Ok(())
    }
}
