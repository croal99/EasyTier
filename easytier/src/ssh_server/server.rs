use std::net::SocketAddr;
use std::sync::Arc;

use russh::server::Server as _;
use tokio::net::TcpListener;

use crate::management_cli::EmbeddedCommandRouter;

use super::config::{self, SshConfig};

pub struct SshServer {
    listen_ip: std::net::IpAddr,
    router: Arc<EmbeddedCommandRouter>,
    config: SshConfig,
}

impl SshServer {
    /// Create a SSH server instance.
    /// The `listen_addr` provides the bind IP; the port is taken from `config.toml` (or its default).
    /// Reads `config.toml` from the working directory; falls back to defaults if absent.
    pub fn new(listen_addr: SocketAddr, router: Arc<EmbeddedCommandRouter>) -> Self {
        let config = config::load_ssh_config();
        Self {
            listen_ip: listen_addr.ip(),
            router,
            config,
        }
    }

    /// Bind TCP listener and serve incoming SSH connections forever.
    pub async fn serve(self) -> anyhow::Result<()> {
        let listen_addr = SocketAddr::new(self.listen_ip, self.config.port);
        let listener = TcpListener::bind(listen_addr).await?;

        let russh_cfg = russh::server::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(600)),
            auth_rejection_time: std::time::Duration::from_secs(1),
            keys: vec![self.config.host_key],
            ..Default::default()
        };

        let russh_cfg = Arc::new(russh_cfg);
        let sh = super::session::ServerHandle::new(self.router, self.config.authorized_keys);

        loop {
            let (socket, _) = listener.accept().await?;
            let russh_cfg = russh_cfg.clone();
            let sh = sh.clone();
            tokio::spawn(async move {
                let res = async {
                    let handler = sh.clone().new_client(socket.peer_addr().ok());
                    let running =
                        russh::server::run_stream(russh_cfg, socket, handler).await?;
                    running.await?;
                    Ok::<(), russh::Error>(())
                }
                .await;
                if let Err(err) = res {
                    tracing::debug!(%err, "ssh session ended with error");
                }
            });
        }
    }
}
