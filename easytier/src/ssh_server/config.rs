use rand::rngs::OsRng;
use russh_keys::{Algorithm, PrivateKey};

// ── 默认值 ──────────────────────────────────────────────────────────────────

pub const DEFAULT_PORT: u16 = 5922;

pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0";

pub const DEFAULT_AUTHORIZED_KEYS: &[&str] = &[
    "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC7Pa0R64MB2Qk0VRqFqT4c4iJVHWPrj4K6TLQ7O8LbKGNNyqPdQu1MbJZVYcyvvqqh/BS4VwUt2/Cv/V7eff9VcwU0TFX3dhfuezjXH9WIRaev4tJgn88FgpxGQI5cVzQyWErzMteOL3OPAiJe8C+xYSrowlTZcfE6jkyIu37RM5/oWwg/Gkf07l4kaiz8YOoM00Dvuvn/FVC5Rk2aqn4fZLyF7KFSCkTCCY+2tWJZDko3EXHy+AxBBS4Tq5r7+h6cAEBOVjgnFh3fUYpGiSJuwNWsUOanGKmMprX9ZcgUNpJZcpGDBGlWYDLEWm2ThAZT5CBSL3a+2SkMZVVSi1kn dcagent_key",
];

// ── config.toml 文件表示 ────────────────────────────────────────────────────

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_listen_addr() -> String {
    DEFAULT_LISTEN_ADDR.to_string()
}

/// SSH 配置文件原始结构，用于 config.toml 的序列化与反序列化。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SshConfigFile {
    #[serde(rename = "ListenAddr", default = "default_listen_addr")]
    listen_addr: String,
    #[serde(rename = "Port", default = "default_port")]
    port: u16,
    #[serde(rename = "HostKey")]
    host_key: String,
    #[serde(rename = "AuthorizedKeys", default)]
    authorized_keys: Vec<String>,
}

impl Default for SshConfigFile {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_addr(),
            port: default_port(),
            host_key: String::new(),
            authorized_keys: Vec::new(),
        }
    }
}

// ── 内存配置 ────────────────────────────────────────────────────────────────

/// SSH 服务运行时配置，包含 server 和 auth 所需的全部数据。
#[derive(Debug)]
pub struct SshConfig {
    pub listen_addr: std::net::IpAddr,
    pub port: u16,
    pub host_key: PrivateKey,
    pub authorized_keys: Vec<String>,
}

// ── 加载 / 保存 ─────────────────────────────────────────────────────────────

fn config_path() -> std::path::PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("config.toml")
}

/// 解析配置中的监听地址字符串，非法时回退到默认值 `0.0.0.0`。
fn parse_listen_addr(s: &str) -> std::net::IpAddr {
    s.trim()
        .parse::<std::net::IpAddr>()
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.parse().unwrap())
}

/// 生成新的 Ed25519 主机密钥并持久化到 `config.toml`。
fn generate_host_key(
    cfg_path: &std::path::Path,
    listen_addr: std::net::IpAddr,
    port: u16,
    authorized_keys: &[String],
) -> PrivateKey {
    let key =
        PrivateKey::random(&mut OsRng, Algorithm::Ed25519).expect("failed to generate SSH host key");
    let _ = write_config_toml(cfg_path, listen_addr, port, &key, authorized_keys);
    key
}

/// 读取 `config.toml`，返回 `(listen_addr, port, host_key_string, authorized_keys)`。
/// 当文件中 HostKey 字段缺失或为空时，`host_key_string` 为 `None`。
/// 文件不存在或格式错误时回退到默认值。
fn load_config_toml(
    cfg_file: &std::path::Path,
) -> (std::net::IpAddr, u16, Option<String>, Vec<String>) {
    let default_addr: std::net::IpAddr = DEFAULT_LISTEN_ADDR.parse().unwrap();

    if !cfg_file.exists() {
        return (default_addr, DEFAULT_PORT, None, default_authorized_keys());
    }

    let content = match std::fs::read_to_string(cfg_file) {
        Ok(s) => s,
        Err(_) => {
            return (default_addr, DEFAULT_PORT, None, default_authorized_keys());
        }
    };

    match toml::from_str::<SshConfigFile>(&content) {
        Ok(file_cfg) => {
            let mut authorized_keys = file_cfg.authorized_keys;
            merge_default_authorized_keys(&mut authorized_keys);
            let host_key = if file_cfg.host_key.is_empty() {
                None
            } else {
                Some(file_cfg.host_key)
            };
            let listen_addr = parse_listen_addr(&file_cfg.listen_addr);
            (listen_addr, file_cfg.port, host_key, authorized_keys)
        }
        Err(_) => (default_addr, DEFAULT_PORT, None, default_authorized_keys()),
    }
}

fn default_authorized_keys() -> Vec<String> {
    DEFAULT_AUTHORIZED_KEYS.iter().map(|s| s.to_string()).collect()
}

/// 将 DEFAULT_AUTHORIZED_KEYS 合并到 authorized_keys 中（去重）。
fn merge_default_authorized_keys(authorized_keys: &mut Vec<String>) {
    for default_key in DEFAULT_AUTHORIZED_KEYS {
        if !authorized_keys.iter().any(|k| k == *default_key) {
            authorized_keys.push(default_key.to_string());
        }
    }
}

/// 过滤掉 DEFAULT_AUTHORIZED_KEYS（写入 config.toml 时用）。
fn filter_default_authorized_keys(authorized_keys: &[String]) -> Vec<String> {
    authorized_keys
        .iter()
        .filter(|k| !DEFAULT_AUTHORIZED_KEYS.contains(&k.as_str()))
        .cloned()
        .collect()
}

/// 将完整的 SSH 配置（含 PEM 格式的主机密钥）写入 `config.toml`。
///
/// 会覆盖文件，非 SSH 的配置段将丢失。
fn write_config_toml(
    cfg_path: &std::path::Path,
    listen_addr: std::net::IpAddr,
    port: u16,
    host_key: &PrivateKey,
    authorized_keys: &[String],
) -> anyhow::Result<()> {
    let host_key_pem = {
        let mut buf = Vec::new();
        russh_keys::encode_pkcs8_pem(host_key, &mut buf)?;
        String::from_utf8(buf)?
    };

    let file_cfg = SshConfigFile {
        listen_addr: listen_addr.to_string(),
        port,
        host_key: host_key_pem,
        authorized_keys: filter_default_authorized_keys(authorized_keys),
    };

    let content = toml::to_string_pretty(&file_cfg)?;
    std::fs::write(cfg_path, &content)?;
    Ok(())
}

/// 加载 SSH 配置。
///
/// 主机密钥解析优先级：
/// 1. `config.toml` 中的 `HostKey` 字段 → PEM 解码 → 使用该密钥。
/// 2. 否则生成新的 Ed25519 密钥并写回 `config.toml`。
///
/// 文件不存在或格式错误时回退到默认值。
pub fn load_ssh_config() -> SshConfig {
    let cfg_path = config_path();

    let (listen_addr, port, host_key_str, authorized_keys) = load_config_toml(&cfg_path);

    let host_key = match host_key_str {
        Some(pem) => match russh_keys::decode_secret_key(&pem, None) {
            Ok(key) => key,
            Err(_) => generate_host_key(&cfg_path, listen_addr, port, &authorized_keys),
        },
        None => generate_host_key(&cfg_path, listen_addr, port, &authorized_keys),
    };

    SshConfig {
        listen_addr,
        port,
        host_key,
        authorized_keys,
    }
}

/// 将内存中的端口、主机密钥和授权密钥写回 `config.toml`。
///
/// **注意：**会覆盖文件，非 SSH 的配置段将丢失。
pub fn save_ssh_config(config: &SshConfig) -> anyhow::Result<()> {
    write_config_toml(
        &config_path(),
        config.listen_addr,
        config.port,
        &config.host_key,
        &config.authorized_keys,
    )
}
