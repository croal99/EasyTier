mod command;
mod rpc_readonly;
mod runtime;

pub use command::{CommandResult, ParsedCommand};
pub use runtime::CoreRuntimeController;

use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::Mutex;

use crate::management_cli::rpc_readonly::ReadOnlyRpcExecutor;

pub struct EmbeddedCommandRouter {
    controller: Arc<Mutex<CoreRuntimeController>>,
}

impl EmbeddedCommandRouter {
    pub fn new(controller: Arc<Mutex<CoreRuntimeController>>) -> Self {
        Self { controller }
    }

    pub async fn execute_line(&self, line: &str) -> anyhow::Result<CommandResult> {
        let cmd = command::parse_line(line)?;
        match cmd {
            ParsedCommand::Noop => Ok(CommandResult::empty()),
            ParsedCommand::Exit => Ok(CommandResult::exit()),
            ParsedCommand::Help => Ok(CommandResult::text(command::help_text())),
            ParsedCommand::Status => {
                let ctrl = self.controller.lock().await;
                Ok(CommandResult::text(ctrl.status_text()))
            }
            ParsedCommand::StartNetwork => {
                let mut ctrl = self.controller.lock().await;
                let msg = ctrl.start_network().await?;
                Ok(CommandResult::text(msg))
            }
            ParsedCommand::StopNetwork => {
                let mut ctrl = self.controller.lock().await;
                let msg = ctrl.stop_network().await?;
                Ok(CommandResult::text(msg))
            }
            ParsedCommand::ConfigShow => {
                let ctrl = self.controller.lock().await;
                Ok(CommandResult::text(ctrl.config_text()))
            }
            ParsedCommand::ConfigSetNetworkName { name } => {
                let mut ctrl = self.controller.lock().await;
                ctrl.set_network_name(name);
                Ok(CommandResult::text("OK".to_string()))
            }
            ParsedCommand::ConfigSetNetworkSecret { secret } => {
                let mut ctrl = self.controller.lock().await;
                ctrl.set_network_secret(secret);
                Ok(CommandResult::text("OK".to_string()))
            }
            ParsedCommand::ConfigSetDhcp { enabled } => {
                let mut ctrl = self.controller.lock().await;
                ctrl.set_dhcp(enabled)?;
                Ok(CommandResult::text("OK".to_string()))
            }
            ParsedCommand::ConfigSetIpv4 { cidr } => {
                let mut ctrl = self.controller.lock().await;
                ctrl.set_ipv4(cidr)?;
                Ok(CommandResult::text("OK".to_string()))
            }
            ParsedCommand::ConfigSetIpv6 { cidr } => {
                let mut ctrl = self.controller.lock().await;
                ctrl.set_ipv6(cidr)?;
                Ok(CommandResult::text("OK".to_string()))
            }
            ParsedCommand::ConfigPeersAdd { url } => {
                let mut ctrl = self.controller.lock().await;
                ctrl.peers_add(url)?;
                Ok(CommandResult::text("OK".to_string()))
            }
            ParsedCommand::ConfigPeersRemove { url } => {
                let mut ctrl = self.controller.lock().await;
                ctrl.peers_remove(url)?;
                Ok(CommandResult::text("OK".to_string()))
            }
            ParsedCommand::ConfigPeersClear => {
                let mut ctrl = self.controller.lock().await;
                ctrl.peers_clear();
                Ok(CommandResult::text("OK".to_string()))
            }
            ParsedCommand::ReadOnlyMgmt { words } => {
                let ctrl = self.controller.lock().await;
                let rpc_addr = ctrl
                    .rpc_addr()
                    .context("network is not started; run start-network first")?;
                drop(ctrl);
                let executor = ReadOnlyRpcExecutor::new(rpc_addr);
                let out = executor.execute(words).await?;
                Ok(CommandResult::text(out))
            }
        }
    }
}
