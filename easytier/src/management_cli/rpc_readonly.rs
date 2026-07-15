use std::net::SocketAddr;

use anyhow::Context as _;

use crate::proto::api::config::ConfigRpcClientFactory;
use crate::proto::api::instance::{
    AclManageRpcClientFactory, CredentialManageRpcClientFactory, PeerManageRpcClientFactory,
    PortForwardManageRpcClientFactory, StatsRpcClientFactory, TcpProxyRpcClientFactory,
    VpnPortalRpcClientFactory,
};
use crate::proto::api::logger::LoggerRpcClientFactory;
use crate::proto::peer_rpc::PeerCenterRpcClientFactory;
use crate::proto::rpc_impl::standalone::StandAloneClient;
use crate::proto::rpc_types::controller::BaseController;
use crate::tunnel::tcp::TcpTunnelConnector;

pub struct ReadOnlyRpcExecutor {
    url: url::Url,
}

impl ReadOnlyRpcExecutor {
    pub fn new(rpc_addr: SocketAddr) -> Self {
        let url: url::Url = format!("tcp://{}", rpc_addr).parse().unwrap();
        Self { url }
    }

    pub async fn execute(&self, words: Vec<String>) -> anyhow::Result<String> {
        let mut client = StandAloneClient::new(TcpTunnelConnector::new(self.url.clone()));
        let head = words.first().map(|s| s.as_str()).unwrap_or("");
        match head {
            "peer" => Self::exec_peer(&mut client, &words).await,
            "peer-center" => Self::exec_peer_center(&mut client).await,
            "route" => Self::exec_route(&mut client, &words).await,
            "node" => Self::exec_node(&mut client, &words).await,
            "vpn-portal" => Self::exec_vpn_portal(&mut client).await,
            "proxy" => Self::exec_proxy(&mut client).await,
            "acl" => Self::exec_acl(&mut client, &words).await,
            "port-forward" => Self::exec_port_forward(&mut client, &words).await,
            "whitelist" => Self::exec_whitelist(&mut client, &words).await,
            "stats" => Self::exec_stats(&mut client, &words).await,
            "logger" => Self::exec_logger(&mut client, &words).await,
            "credential" => Self::exec_credential(&mut client, &words).await,
            _ => anyhow::bail!("unknown management command: {}", words.join(" ")),
        }
    }

    async fn exec_peer(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("list");
        match sub {
            "list" => {
                let peer = client
                    .scoped_client::<PeerManageRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get peer client")?;
                let resp = peer
                    .list_peer(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            "ipv6" => {
                let peer = client
                    .scoped_client::<PeerManageRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get peer client")?;
                let resp = peer
                    .list_public_ipv6_info(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            _ => anyhow::bail!("unknown peer subcommand: {sub}"),
        }
    }

    async fn exec_peer_center(
        client: &mut StandAloneClient<TcpTunnelConnector>,
    ) -> anyhow::Result<String> {
        let pc = client
            .scoped_client::<PeerCenterRpcClientFactory<BaseController>>("".to_string())
            .await
            .context("failed to get peer-center client")?;
        let resp = pc
            .get_global_peer_map(BaseController::default(), Default::default())
            .await?;
        Ok(format!("{:#?}", resp))
    }

    async fn exec_route(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("list");
        match sub {
            "list" => {
                let peer = client
                    .scoped_client::<PeerManageRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get peer client")?;
                let resp = peer
                    .list_route(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            "dump" => {
                let peer = client
                    .scoped_client::<PeerManageRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get peer client")?;
                let resp = peer
                    .dump_route(BaseController::default(), Default::default())
                    .await?;
                Ok(resp.result)
            }
            _ => anyhow::bail!("unknown route subcommand: {sub}"),
        }
    }

    async fn exec_node(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("info");
        match sub {
            "info" | "show" => {
                let peer = client
                    .scoped_client::<PeerManageRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get peer client")?;
                let resp = peer
                    .show_node_info(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            "config" => {
                let cfg = client
                    .scoped_client::<ConfigRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get config client")?;
                let resp = cfg
                    .get_config(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            _ => anyhow::bail!("unknown node subcommand: {sub}"),
        }
    }

    async fn exec_vpn_portal(
        client: &mut StandAloneClient<TcpTunnelConnector>,
    ) -> anyhow::Result<String> {
        let vp = client
            .scoped_client::<VpnPortalRpcClientFactory<BaseController>>("".to_string())
            .await
            .context("failed to get vpn-portal client")?;
        let resp = vp
            .get_vpn_portal_info(BaseController::default(), Default::default())
            .await?;
        Ok(format!("{:#?}", resp))
    }

    async fn exec_proxy(
        client: &mut StandAloneClient<TcpTunnelConnector>,
    ) -> anyhow::Result<String> {
        let mut all = Vec::new();
        for client_type in ["tcp", "kcp_src", "kcp_dst", "quic_src", "quic_dst"] {
            let proxy = client
                .scoped_client::<TcpProxyRpcClientFactory<BaseController>>(client_type.to_string())
                .await
                .context("failed to get proxy client")?;
            let resp = proxy
                .list_tcp_proxy_entry(BaseController::default(), Default::default())
                .await?;
            all.push(resp);
        }
        Ok(format!("{:#?}", all))
    }

    async fn exec_acl(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("stats");
        match sub {
            "stats" => {
                let acl = client
                    .scoped_client::<AclManageRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get acl client")?;
                let resp = acl
                    .get_acl_stats(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            _ => anyhow::bail!("unknown acl subcommand: {sub}"),
        }
    }

    async fn exec_port_forward(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("list");
        match sub {
            "list" => {
                let pf = client
                    .scoped_client::<PortForwardManageRpcClientFactory<BaseController>>(
                        "".to_string(),
                    )
                    .await
                    .context("failed to get port-forward client")?;
                let resp = pf
                    .list_port_forward(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            _ => anyhow::bail!("unknown port-forward subcommand: {sub}"),
        }
    }

    async fn exec_whitelist(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("show");
        match sub {
            "show" => {
                let acl = client
                    .scoped_client::<AclManageRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get acl client")?;
                let resp = acl
                    .get_whitelist(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            _ => anyhow::bail!("unknown whitelist subcommand: {sub}"),
        }
    }

    async fn exec_stats(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("show");
        match sub {
            "show" => {
                let stats = client
                    .scoped_client::<StatsRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get stats client")?;
                let resp = stats
                    .get_stats(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            "prometheus" => {
                let stats = client
                    .scoped_client::<StatsRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get stats client")?;
                let resp = stats
                    .get_prometheus_stats(BaseController::default(), Default::default())
                    .await?;
                Ok(resp.prometheus_text)
            }
            _ => anyhow::bail!("unknown stats subcommand: {sub}"),
        }
    }

    async fn exec_logger(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("get");
        match sub {
            "get" => {
                let logger = client
                    .scoped_client::<LoggerRpcClientFactory<BaseController>>("".to_string())
                    .await
                    .context("failed to get logger client")?;
                let resp = logger
                    .get_logger_config(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            _ => anyhow::bail!("unknown logger subcommand: {sub}"),
        }
    }

    async fn exec_credential(
        client: &mut StandAloneClient<TcpTunnelConnector>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let sub = words.get(1).map(|s| s.as_str()).unwrap_or("list");
        match sub {
            "list" => {
                let cred = client
                    .scoped_client::<CredentialManageRpcClientFactory<BaseController>>(
                        "".to_string(),
                    )
                    .await
                    .context("failed to get credential client")?;
                let resp = cred
                    .list_credentials(BaseController::default(), Default::default())
                    .await?;
                Ok(format!("{:#?}", resp))
            }
            _ => anyhow::bail!("unknown credential subcommand: {sub}"),
        }
    }
}
