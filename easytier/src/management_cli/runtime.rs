use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::Mutex;

use crate::common::config::{ConfigFileControl, ConfigLoader, NetworkIdentity, PeerConfig, TomlConfigLoader};
use crate::instance_manager::NetworkInstanceManager;
use crate::rpc_service::ApiRpcServer;
use crate::tunnel::tcp::TcpTunnelListener;
use crate::utils::find_free_tcp_port;

pub struct CoreRuntimeController {
    config: TomlConfigLoader,
    manager: Option<Arc<NetworkInstanceManager>>,
    rpc_server: Option<ApiRpcServer<TcpTunnelListener>>,
    rpc_addr: Option<SocketAddr>,
    instance_id: Option<uuid::Uuid>,
}

impl CoreRuntimeController {
    pub fn new() -> Self {
        Self {
            config: TomlConfigLoader::default(),
            manager: None,
            rpc_server: None,
            rpc_addr: None,
            instance_id: None,
        }
    }

    pub fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::new()))
    }

    pub fn rpc_addr(&self) -> Option<SocketAddr> {
        self.rpc_addr
    }

    pub fn set_network_name(&mut self, name: String) {
        let old = self.config.get_network_identity();
        let secret = old.network_secret.unwrap_or_default();
        self.config.set_network_identity(NetworkIdentity::new(name, secret));
    }

    pub fn set_network_secret(&mut self, secret: String) {
        let old = self.config.get_network_identity();
        self.config
            .set_network_identity(NetworkIdentity::new(old.network_name, secret));
    }

    pub fn set_dhcp(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.config.set_dhcp(enabled);
        if enabled {
            self.config.set_ipv4(None);
            self.config.set_ipv6(None);
        }
        Ok(())
    }

    pub fn set_ipv4(&mut self, cidr: Option<String>) -> anyhow::Result<()> {
        match cidr {
            None => self.config.set_ipv4(None),
            Some(text) => {
                let addr = text
                    .parse::<cidr::Ipv4Inet>()
                    .with_context(|| format!("invalid ipv4 cidr: {text}"))?;
                self.config.set_ipv4(Some(addr));
                self.config.set_dhcp(false);
            }
        }
        Ok(())
    }

    pub fn set_ipv6(&mut self, cidr: Option<String>) -> anyhow::Result<()> {
        match cidr {
            None => self.config.set_ipv6(None),
            Some(text) => {
                let addr = text
                    .parse::<cidr::Ipv6Inet>()
                    .with_context(|| format!("invalid ipv6 cidr: {text}"))?;
                self.config.set_ipv6(Some(addr));
                self.config.set_dhcp(false);
            }
        }
        Ok(())
    }

    pub fn peers_add(&mut self, url: String) -> anyhow::Result<()> {
        let uri = url::Url::parse(&url).with_context(|| format!("invalid peer url: {url}"))?;
        let mut peers = self.config.get_peers();
        if peers.iter().any(|p| p.uri == uri) {
            return Ok(());
        }
        peers.push(PeerConfig {
            uri,
            peer_public_key: None,
        });
        self.config.set_peers(peers);
        Ok(())
    }

    pub fn peers_remove(&mut self, url: String) -> anyhow::Result<()> {
        let uri = url::Url::parse(&url).with_context(|| format!("invalid peer url: {url}"))?;
        let peers = self
            .config
            .get_peers()
            .into_iter()
            .filter(|p| p.uri != uri)
            .collect::<Vec<_>>();
        self.config.set_peers(peers);
        Ok(())
    }

    pub fn set_no_tun(&mut self, enabled: bool) {
        let mut flags = self.config.get_flags();
        flags.no_tun = enabled;
        self.config.set_flags(flags);
    }

    pub fn get_no_tun(&self) -> bool {
        self.config.get_flags().no_tun
    }

    pub fn peers_clear(&mut self) {
        self.config.set_peers(Vec::new());
    }

    pub fn networks_add(&mut self, cidr: String) -> anyhow::Result<()> {
        crate::launcher::add_proxy_network_to_config(&cidr, &self.config)
    }

    pub fn networks_remove(&mut self, cidr: String) -> anyhow::Result<()> {
        let parsed: cidr::Ipv4Cidr = cidr
            .parse()
            .with_context(|| format!("invalid CIDR: {cidr}"))?;
        self.config.remove_proxy_cidr(parsed);
        Ok(())
    }

    pub fn networks_clear(&mut self) {
        self.config.clear_proxy_cidrs();
    }

    /// Render current in-memory config state as human-readable text.
    pub fn config_text(&self) -> String {
        let identity = self.config.get_network_identity();
        let dhcp = self.config.get_dhcp();
        let peers = self.config.get_peers();
        let ipv4 = self.config.get_ipv4();
        let ipv6 = self.config.get_ipv6();
        let networks = self.config.get_proxy_cidrs();

        let mut lines = Vec::new();
        lines.push(format!("network-name: {}", identity.network_name));
        lines.push(format!(
            "network-secret: {}",
            identity.network_secret.as_deref().unwrap_or("")
        ));
        lines.push(format!("dhcp: {}", dhcp));
        lines.push(format!("no-tun: {}", self.get_no_tun()));
        lines.push(format!(
            "ipv4: {}",
            ipv4.map(|v| v.to_string()).unwrap_or_else(|| "off".to_string())
        ));
        lines.push(format!(
            "ipv6: {}",
            ipv6.map(|v| v.to_string()).unwrap_or_else(|| "off".to_string())
        ));
        lines.push(format!("peers: {}", peers.len()));
        for (idx, p) in peers.iter().enumerate() {
            lines.push(format!("  {}: {}", idx + 1, p.uri));
        }
        lines.push(format!("networks: {}", networks.len()));
        for (idx, n) in networks.iter().enumerate() {
            match &n.mapped_cidr {
                Some(mapped) => lines.push(format!("  {}: {}->{}", idx + 1, n.cidr, mapped)),
                None => lines.push(format!("  {}: {}", idx + 1, n.cidr)),
            }
        }
        lines.join("\n")
    }

    pub fn status_text(&self) -> String {
        let running = self.instance_id.is_some();
        let mut lines = vec![format!("network-running: {}", running)];
        if let Some(id) = self.instance_id {
            lines.push(format!("instance-id: {}", id));
        }
        if let Some(addr) = self.rpc_addr {
            lines.push(format!("rpc-addr: {}", addr));
        }
        lines.push(String::new());
        lines.push(self.config_text());
        lines.join("\n")
    }

    /// Start a loopback RPC portal and then start a single network instance.
    pub async fn start_network(&mut self) -> anyhow::Result<String> {
        if self.instance_id.is_some() {
            return Ok("network already started".to_string());
        }

        let manager = self
            .manager
            .get_or_insert_with(|| Arc::new(NetworkInstanceManager::new()))
            .clone();

        let port = find_free_tcp_port(15888..15900).context("no free port for rpc portal")?;
        let rpc_addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);
        let rpc_url: url::Url = format!("tcp://{}", rpc_addr).parse().unwrap();
        let tunnel = TcpTunnelListener::new(rpc_url);

        let rpc_server = ApiRpcServer::from_tunnel(tunnel, manager.clone()).serve().await?;
        self.rpc_server = Some(rpc_server);
        self.rpc_addr = Some(rpc_addr);

        let instance_id = manager.run_network_instance(
            self.config.clone(),
            true,
            ConfigFileControl::STATIC_CONFIG,
        )?;
        self.instance_id = Some(instance_id);

        Ok(format!("started: instance-id={instance_id} rpc-addr={rpc_addr}"))
    }

    pub async fn stop_network(&mut self) -> anyhow::Result<String> {
        let Some(instance_id) = self.instance_id.take() else {
            return Ok("network is not started".to_string());
        };

        if let Some(manager) = self.manager.as_ref() {
            manager.delete_network_instance(vec![instance_id])?;
        }

        self.rpc_server = None;
        self.rpc_addr = None;

        Ok(format!("stopped: instance-id={instance_id}"))
    }
}
