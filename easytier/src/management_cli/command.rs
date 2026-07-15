use anyhow::Context as _;

pub struct CommandResult {
    pub output: String,
    pub should_exit: bool,
}

impl CommandResult {
    pub fn empty() -> Self {
        Self {
            output: String::new(),
            should_exit: false,
        }
    }

    pub fn text(output: String) -> Self {
        Self {
            output,
            should_exit: false,
        }
    }

    pub fn exit() -> Self {
        Self {
            output: String::new(),
            should_exit: true,
        }
    }
}

pub enum ParsedCommand {
    Noop,
    Exit,
    Help,
    Status,
    StartNetwork,
    StopNetwork,

    ConfigShow,
    ConfigSetNetworkName { name: String },
    ConfigSetNetworkSecret { secret: String },
    ConfigSetDhcp { enabled: bool },
    ConfigSetIpv4 { cidr: Option<String> },
    ConfigSetIpv6 { cidr: Option<String> },
    ConfigPeersAdd { url: String },
    ConfigPeersRemove { url: String },
    ConfigPeersClear,

    ReadOnlyMgmt { words: Vec<String> },
}

pub fn help_text() -> String {
    [
        "Built-in commands:",
        "  help",
        "  status",
        "  start-network",
        "  stop-network",
        "",
        "Config commands:",
        "  config show",
        "  config set network-name <name>",
        "  config set network-secret <secret>",
        "  config set dhcp <on|off>",
        "  config set ipv4 <CIDR>|off",
        "  config set ipv6 <CIDR>|off",
        "  config peers add <url>",
        "  config peers remove <url>",
        "  config peers clear",
        "",
        "Read-only management commands (available after start-network):",
        "  peer list|ipv6",
        "  peer-center",
        "  route list|dump",
        "  node|node info|node config",
        "  vpn-portal",
        "  proxy",
        "  acl stats",
        "  port-forward list",
        "  whitelist show",
        "  stats show|prometheus",
        "  logger get",
        "  credential list",
        "",
        "Note:",
        "  Read-only management commands will fail until you run start-network.",
        "",
        "Exit:",
        "  exit|quit",
    ]
    .join("\n")
}

pub fn parse_line(line: &str) -> anyhow::Result<ParsedCommand> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(ParsedCommand::Noop);
    }

    let words = line
        .split_whitespace()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let Some(head) = words.first().map(|s| s.as_str()) else {
        return Ok(ParsedCommand::Noop);
    };

    match head {
        "exit" | "quit" => Ok(ParsedCommand::Exit),
        "help" => Ok(ParsedCommand::Help),
        "status" => Ok(ParsedCommand::Status),
        "start-network" => Ok(ParsedCommand::StartNetwork),
        "stop-network" => Ok(ParsedCommand::StopNetwork),
        "config" => parse_config(&words),
        _ => Ok(ParsedCommand::ReadOnlyMgmt { words }),
    }
}

fn parse_config(words: &[String]) -> anyhow::Result<ParsedCommand> {
    let sub = words.get(1).map(|s| s.as_str()).unwrap_or("");
    match sub {
        "show" => Ok(ParsedCommand::ConfigShow),
        "set" => parse_config_set(words),
        "peers" => parse_config_peers(words),
        _ => anyhow::bail!("unknown config subcommand: {sub}"),
    }
}

fn parse_config_set(words: &[String]) -> anyhow::Result<ParsedCommand> {
    let key = words
        .get(2)
        .map(|s| s.as_str())
        .context("missing config key")?;
    match key {
        "network-name" => {
            let name = words.get(3).cloned().context("missing network-name")?;
            Ok(ParsedCommand::ConfigSetNetworkName { name })
        }
        "network-secret" => {
            let secret = words.get(3).cloned().context("missing network-secret")?;
            Ok(ParsedCommand::ConfigSetNetworkSecret { secret })
        }
        "dhcp" => {
            let value = words.get(3).map(|s| s.as_str()).unwrap_or("");
            let enabled = match value {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                _ => anyhow::bail!("invalid dhcp value: {value} (use on|off)"),
            };
            Ok(ParsedCommand::ConfigSetDhcp { enabled })
        }
        "ipv4" => {
            let value = words.get(3).map(|s| s.as_str()).unwrap_or("");
            if value == "off" || value.is_empty() {
                Ok(ParsedCommand::ConfigSetIpv4 { cidr: None })
            } else {
                Ok(ParsedCommand::ConfigSetIpv4 {
                    cidr: Some(value.to_string()),
                })
            }
        }
        "ipv6" => {
            let value = words.get(3).map(|s| s.as_str()).unwrap_or("");
            if value == "off" || value.is_empty() {
                Ok(ParsedCommand::ConfigSetIpv6 { cidr: None })
            } else {
                Ok(ParsedCommand::ConfigSetIpv6 {
                    cidr: Some(value.to_string()),
                })
            }
        }
        _ => anyhow::bail!("unknown config key: {key}"),
    }
}

fn parse_config_peers(words: &[String]) -> anyhow::Result<ParsedCommand> {
    let action = words.get(2).map(|s| s.as_str()).unwrap_or("");
    match action {
        "add" => Ok(ParsedCommand::ConfigPeersAdd {
            url: words.get(3).cloned().context("missing peer url")?,
        }),
        "remove" => Ok(ParsedCommand::ConfigPeersRemove {
            url: words.get(3).cloned().context("missing peer url")?,
        }),
        "clear" => Ok(ParsedCommand::ConfigPeersClear),
        _ => anyhow::bail!("unknown config peers action: {action}"),
    }
}
