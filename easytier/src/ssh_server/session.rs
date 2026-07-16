use std::sync::Arc;

use crate::management_cli::EmbeddedCommandRouter;
use russh::keys::HashAlg;

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

pub struct SessionHandle {
    router: Arc<EmbeddedCommandRouter>,
    buf: Vec<u8>,
    shell_channel: Option<russh::ChannelId>,
    last_was_cr: bool,
    /// Command history, most recent command last.
    history: Vec<String>,
    /// Current position in history navigation; None means at the "fresh" line.
    history_index: Option<usize>,
    /// Saved input line before entering history navigation.
    saved_buf: Vec<u8>,
    /// ESC-sequence parser state.  None = normal mode.
    esc_state: Option<EscState>,
    /// Remote address of the connecting client, for logging purposes.
    remote_addr: Option<std::net::SocketAddr>,
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
    fn new(router: Arc<EmbeddedCommandRouter>, remote_addr: Option<std::net::SocketAddr>) -> Self {
        Self {
            router,
            buf: Vec::new(),
            shell_channel: None,
            last_was_cr: false,
            history: Vec::new(),
            history_index: None,
            saved_buf: Vec::new(),
            esc_state: None,
            remote_addr,
            pty_allocated: false,
            pty_echo: false,
        }
    }

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

    /// Execute one completed command line and print the result.
    async fn execute_shell_line(
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
                    session.exit_status_request(channel, 0)?;
                    session.close(channel)?;
                    self.shell_channel = None;
                    return Ok(true);
                }
            }
            Err(e) => {
                Self::write_text(session, channel, format!("ERROR: {e}"))?;
            }
        }

        Self::write_prompt(session, channel)?;
        Ok(false)
    }
}

impl russh::server::Server for ServerHandle {
    type Handler = SessionHandle;

    fn new_client(&mut self, peer_addr: Option<std::net::SocketAddr>) -> SessionHandle {
        SessionHandle::new(self.router.clone(), peer_addr)
    }
}

impl russh::server::Handler for SessionHandle {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &russh::keys::PublicKey,
    ) -> Result<russh::server::Auth, Self::Error> {
        if super::auth::is_authorized_public_key(public_key) {
            tracing::info!(
                user = %user,
                remote = ?self.remote_addr,
                key_fingerprint = %public_key.fingerprint(HashAlg::Sha256),
                key_type = %public_key.algorithm(),
                "SSH auth accepted"
            );
            Ok(russh::server::Auth::Accept)
        } else {
            tracing::warn!(
                user = %user,
                remote = ?self.remote_addr,
                key_fingerprint = %public_key.fingerprint(HashAlg::Sha256),
                key_type = %public_key.algorithm(),
                "SSH auth rejected: unauthorized public key"
            );
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
        name: &str,
        value: &str,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        tracing::debug!(
            remote = ?self.remote_addr,
            name = %name,
            value = %value,
            "env request"
        );
        session.channel_success(channel)?;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: russh::ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        modes: &[(russh::Pty, u32)],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.pty_allocated = true;

        // Extract the ECHO flag from PTY modes.
        // ECHO=1 means the server should echo; ECHO=0 means the client does local echo.
        self.pty_echo = modes
            .iter()
            .any(|&(code, val)| code == russh::Pty::ECHO && val != 0);

        tracing::info!(
            remote = ?self.remote_addr,
            term = %term,
            cols = col_width,
            rows = row_height,
            pix_w = pix_width,
            pix_h = pix_height,
            pty_echo = self.pty_echo,
            mode_count = modes.len(),
            "PTY allocated"
        );
        session.channel_success(channel)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: russh::ChannelId,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        tracing::debug!(
            remote = ?self.remote_addr,
            cols = col_width,
            rows = row_height,
            pix_w = pix_width,
            pix_h = pix_height,
            "terminal window changed"
        );
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: russh::ChannelId,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.shell_channel = Some(channel);
        session.channel_success(channel)?;
        Self::write_prompt(session, channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;

        let line = String::from_utf8_lossy(data).trim().to_string();
        let res = self.router.execute_line(&line).await;
        let out = match res {
            Ok(r) => r.output,
            Err(e) => format!("ERROR: {e}"),
        };
        Self::write_text(session, channel, out)?;

        session.exit_status_request(channel, 0)?;
        session.close(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if self.shell_channel != Some(channel) {
            return Ok(());
        }

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
                    if self.execute_shell_line(session, channel, line).await? {
                        break;
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
                    if self.execute_shell_line(session, channel, line).await? {
                        break;
                    }
                }
                0x08 | 0x7f => {
                    self.last_was_cr = false;
                    if self.buf.pop().is_some() {
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
                    // Ctrl+D = EOF – close session if line is empty, else discard.
                    self.last_was_cr = false;
                    if self.buf.is_empty() {
                        Self::write_raw(session, channel, "\r\n")?;
                        session.exit_status_request(channel, 0)?;
                        session.close(channel)?;
                        self.shell_channel = None;
                        break;
                    }
                }
                byte => {
                    self.last_was_cr = false;
                    if byte.is_ascii_graphic() || byte == b' ' {
                        self.buf.push(byte);
                        if should_echo {
                            // Single ASCII byte: safe to interpret as UTF-8.
                            let mut buf = [0u8; 4];
                            let ch_str = (byte as char).encode_utf8(&mut buf);
                            Self::write_raw(session, channel, ch_str)?;
                        }
                    }
                    // Non-printable/non-ASCII bytes are silently ignored.
                }
            }
        }

        Ok(())
    }
}
