use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rand::rngs::OsRng;
use russh::server::Server as _;
use russh_keys::{Algorithm, PrivateKey};
use tokio::net::TcpListener;

use crate::management_cli::EmbeddedCommandRouter;

/// Resolve the default path for persisting the SSH host key
/// (next to the config file in the current working directory).
fn default_host_key_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("ssh_host_key")
}

/// Load an existing host key from disk, or generate a new one and persist it.
fn load_or_generate_host_key(key_path: &std::path::Path) -> anyhow::Result<PrivateKey> {
    if key_path.exists() {
        tracing::info!(path = %key_path.display(), "loading persisted SSH host key");
        russh_keys::load_secret_key(key_path, None)
            .map_err(|e| anyhow::anyhow!("failed to load host key from {}: {e}", key_path.display()))
    } else {
        tracing::info!(path = %key_path.display(), "generating new SSH host key");
        let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)?;
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(key_path)?;
        russh_keys::encode_pkcs8_pem(&key, &mut file)
            .map_err(|e| anyhow::anyhow!("failed to write host key to {}: {e}", key_path.display()))?;
        Ok(key)
    }
}

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
        let key_path = default_host_key_path();
        let host_key = load_or_generate_host_key(&key_path)?;
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
