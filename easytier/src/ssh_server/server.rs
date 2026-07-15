use std::net::SocketAddr;
use std::sync::Arc;

use rand::rngs::OsRng;
use russh::server::Server as _;
use tokio::net::TcpListener;

use crate::management_cli::EmbeddedCommandRouter;

pub struct SshServer {
    listen_addr: SocketAddr,
    router: Arc<EmbeddedCommandRouter>,
}

impl SshServer {
    /// Create a SSH server instance that will accept connections and route commands to the embedded CLI.
    pub fn new(listen_addr: SocketAddr, router: Arc<EmbeddedCommandRouter>) -> Self {
        Self {
            listen_addr,
            router,
        }
    }

    /// Bind TCP listener and serve incoming SSH connections forever.
    pub async fn serve(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        let host_key = russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519)?;
        let config = russh::server::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(600)),
            auth_rejection_time: std::time::Duration::from_secs(1),
            keys: vec![host_key],
            ..Default::default()
        };

        let config = Arc::new(config);
        let sh = super::session::ServerHandle::new(self.router);

        loop {
            let (socket, _) = listener.accept().await?;
            let config = config.clone();
            let sh = sh.clone();
            tokio::spawn(async move {
                let res = async {
                    let handler = sh.clone().new_client(socket.peer_addr().ok());
                    let running = russh::server::run_stream(config, socket, handler).await?;
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
