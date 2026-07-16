use std::sync::Arc;

use crate::management_cli::EmbeddedCommandRouter;

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

pub struct SessionHandle {
    router: Arc<EmbeddedCommandRouter>,
    buf: Vec<u8>,
    shell_channel: Option<russh::ChannelId>,
    last_was_cr: bool,
}

impl SessionHandle {
    /// Create a per-connection session handler.
    fn new(router: Arc<EmbeddedCommandRouter>) -> Self {
        Self {
            router,
            buf: Vec::new(),
            shell_channel: None,
            last_was_cr: false,
        }
    }

    fn prompt() -> &'static str {
        "bluenet> "
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

    /// Execute one completed command line and print the result.
    async fn execute_shell_line(
        &mut self,
        session: &mut russh::server::Session,
        channel: russh::ChannelId,
        line: String,
    ) -> Result<bool, russh::Error> {
        let line = line.trim().to_string();
        if line.is_empty() {
            Self::write_prompt(session, channel)?;
            return Ok(false);
        }

        let res = self.router.execute_line(&line).await;
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

    async fn pty_request(
        &mut self,
        channel: russh::ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
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

        for byte in data {
            match *byte {
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
                        // Erase the previous character on a basic VT-compatible terminal.
                        Self::write_raw(session, channel, "\u{8} \u{8}")?;
                    }
                }
                byte => {
                    self.last_was_cr = false;
                    self.buf.push(byte);
                    let text = String::from_utf8_lossy(&[byte]).to_string();
                    Self::write_raw(session, channel, &text)?;
                }
            }
        }

        Ok(())
    }
}
