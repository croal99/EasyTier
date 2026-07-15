# EasyTier：先启动 SSH、按需 start-network/stop-network 的实现计划

## Summary

目标：让 `easytier-core` 启动时**只启动 SSH Server**（默认 `127.0.0.1:2222`），不自动启动 EasyTier 核心网络；登录后通过内置命令设置 `network-name`、`network-secret`、`dhcp/ipv4/ipv6`、`peers` 等配置，并使用 `start-network` 启动核心网络功能、`stop-network` 关闭核心网络功能（SSH 仍保持在线）。

验收标准（最小闭环）：
- `cargo build -p easytier --bin easytier-core` 通过（必要时设置 `PROTOC`）。
- 运行 `easytier-core` 后 SSH 可连接，通过公钥认证进入交互式 shell。
- SSH shell 内命令可用：`help/status/config show/config set .../config peers .../start-network/stop-network/exit`。
- `start-network` 会启动单实例网络 + loopback RPC portal（`127.0.0.1:<auto>`），`stop-network` 会停止实例并释放 RPC。

## Current State Analysis（基于仓库现状）

已存在代码骨架（但目前会因 russh 版本 API 不匹配而无法编译）：
- SSH Server 模块：`easytier/src/ssh_server/`（`auth.rs/server.rs/session.rs`）
- 内置命令与按需运行控制器：`easytier/src/management_cli/`（`command.rs/mod.rs/runtime.rs/rpc_readonly.rs`）
- `easytier-core` 入口：`easytier/src/core.rs` 已改为默认启动 SSH，并在退出时尝试 `stop_network`

当前阻塞点（需要修复才能闭环）：
- `russh` 0.50 API 适配问题：
  - `Server::new_client` 在 0.50 是同步 `fn new_client(...) -> Handler`，当前实现用了 `async fn`
  - `Handler` 方法签名不匹配：0.50 使用 `&mut self` + `&mut Session`，并返回 `Result<...>`（或 `impl Future`），当前实现是旧风格 `(Self, Session)`
  - `Auth::Reject` 在 0.50 是结构体变体 `Reject { proceed_with_methods: Option<MethodSet> }`
  - `Config` 字段在 0.50 为 `inactivity_timeout` 等，当前写了不存在的 `connection_timeout`
  - host key 生成与类型：0.50 `Config.keys: Vec<russh::keys::PrivateKey>`，当前写了 `russh_keys::key::KeyPair`
- `CoreRuntimeController` 有两个编译/逻辑问题：
  - `ApiRpcServer::serve(self) -> Result<Self, _>` 会 move，当前写法会导致 `rpc_server` 被 move 后又使用
  - `config_text()` 里通过 `flags.dhcp` 取 DHCP 状态，但 DHCP 实际是 `TomlConfigLoader::get_dhcp()`（并非 flags 字段）

## Assumptions & Decisions

- SSH 鉴权：使用**硬编码测试公钥**（PoC），后续再扩展读取 `authorized_keys` 文件/热更新。
- SSH 监听：默认 `127.0.0.1:2222`（可通过现有 `--ssh-listen/ET_SSH_LISTEN` 修改）。
- 内置 config 命令范围：先做**最小集**（`network-name/secret`、`dhcp/ipv4/ipv6`、`peers`）。
- 网络实例：仅支持**单实例**；配置仅保存在内存态（`TomlConfigLoader`），不落盘。
- 安全约束：RPC portal 仅绑定 loopback（`127.0.0.1`），且仅在 `start-network` 后启动。

## Proposed Changes（按文件）

### 1) 修复 SSH Server：适配 russh 0.50（核心阻塞项）

#### 1.1 `easytier/src/ssh_server/server.rs`

目标：
- 使用 `russh::server::Config` 的正确字段（如 `inactivity_timeout`）
- 生成并注入 host key：`russh::keys::PrivateKey::random(..., Algorithm::Ed25519)`
- `run_stream()` 返回 `RunningSession`，需要在 spawned task 内 `.await` 直到会话结束

具体改动：
- 替换 `connection_timeout` 为 `inactivity_timeout`（或直接用默认 + 配置 `inactivity_timeout: Some(…)`）
- 替换 `russh_keys::key::KeyPair::generate_ed25519()` 为：
  - `use rand_core::OsRng;`
  - `russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519)?`
- spawned task 内逻辑调整为：
  - `let running = russh::server::run_stream(config, socket, handler).await?;`
  - `running.await?;`

#### 1.2 `easytier/src/ssh_server/session.rs`

目标：
- 按 russh 0.50 的 `server::Server`/`server::Handler` trait 重写
- 支持 `shell + exec + pty_request + data` 的最小交互
- 输出使用 `russh::CryptoVec`（`session.data(channel, CryptoVec::from(...))`）

具体改动：
- 移除旧版 `async_trait` 用法（不依赖宏也可直接 `async fn` 实现 trait）
- `impl russh::server::Server for ServerHandle`：
  - `fn new_client(&mut self, peer_addr: Option<SocketAddr>) -> SessionHandle`
- `impl russh::server::Handler for SessionHandle`：
  - `type Error = russh::Error` 或 `anyhow::Error`（需满足 `From<russh::Error> + Send`；建议先用 `russh::Error` 简化）
  - `async fn auth_publickey(&mut self, user: &str, key: &ssh_key::PublicKey) -> Result<Auth, Self::Error>`
  - `async fn channel_open_session(&mut self, channel: Channel<Msg>, session: &mut Session) -> Result<bool, Self::Error>`
  - `async fn pty_request(&mut self, ... , session: &mut Session) -> Result<(), Self::Error>`：`session.channel_success(channel)`
  - `async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error>`：记录 `shell_channel`，`channel_success`，发送 prompt
  - `async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error>`：执行一行命令，写回 stdout，`exit_status_request` + `close`
  - `async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error>`：按行缓冲，逐行执行
- `exit`：在 shell 模式下收到 `should_exit` 时关闭 channel；exec 模式下始终结束

#### 1.3 `easytier/src/ssh_server/auth.rs`

目标：
- 适配公钥类型：使用 `russh::keys::PublicKey`（即 `ssh_key::PublicKey`）
- 修复 fingerprint API（需要 `HashAlg` 参数）

具体改动：
- 将 `use russh_keys::key::PublicKey;` 改为 `use russh::keys::{HashAlg, PublicKey};`
- 授权判断逻辑改为：
  - 解析硬编码 authorized keys（每行 split whitespace，取 base64 token）
  - `russh::keys::parse_public_key_base64(token)` 得到 `PublicKey`
  - 用 `fingerprint(HashAlg::Sha256)` 做等价比较（或直接比较 `PublicKey` 序列化内容）

### 2) 修复按需启动控制器：start-network/stop-network 与配置展示

#### 2.1 `easytier/src/management_cli/runtime.rs`

目标：
- 修复 `ApiRpcServer::serve` move 语义导致的编译错误
- 修复 DHCP 状态展示
- 保持 `start-network` 行为：启动 RPC portal（loopback + auto port）→ 启动 instance → 返回状态文本

具体改动：
- `start_network`：
  - 替换：
    - `let mut rpc_server = ApiRpcServer::from_tunnel(...); rpc_server.serve().await?; self.rpc_server = Some(rpc_server);`
  - 为：
    - `let rpc_server = ApiRpcServer::from_tunnel(...).serve().await?; self.rpc_server = Some(rpc_server);`
- `config_text`：
  - 替换 `flags.dhcp` 为 `self.config.get_dhcp()`
  - `flags` 仍可保留用于其它 flag 展示，但不用于 DHCP

### 3) 内置命令路由（已基本满足需求，仅补齐文案与错误提示）

#### 3.1 `easytier/src/management_cli/command.rs`

现状已满足最小集需求：
- `config set network-name/network-secret/dhcp/ipv4/ipv6`
- `config peers add/remove/clear`
- `start-network/stop-network/status/help/exit`

计划补强：
- `help_text()` 中明确 “network 未启动时只读管理命令不可用”
- 对 `ReadOnlyMgmt` 在未启动时的错误信息保持一致（当前已在 router 里做 `context("network is not started; run start-network first")`）

### 4) core 启动行为核对（确保符合“先 SSH，后 start-network”）

#### 4.1 `easytier/src/core.rs`

现状符合需求：仅在 `cli.ssh_server` 时启动 SSH，并在进程退出时 `stop_network`。

计划补强（非强制，但建议）：
- SSH `serve()` 的 spawned task 对错误做日志输出，避免静默失败（例如 bind 失败时）

### 5) 验证与最小回归测试

#### 5.1 构建验证

- `env PROTOC=/tmp/protoc-35.1/bin/protoc cargo build -p easytier --bin easytier-core`
- 若仅检查：`env PROTOC=... cargo check -p easytier --bin easytier-core`

#### 5.2 手工验收步骤（本机）

1) 启动：
- `env PROTOC=/tmp/protoc-35.1/bin/protoc ./target/debug/easytier-core --ssh-server --ssh-listen 127.0.0.1:2222`

2) 连接（示例）：
- `ssh -p 2222 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null <user>@127.0.0.1`

3) SSH 内命令验证：
- `help`
- `status`
- `config show`
- `config set network-name demo`
- `config set network-secret 123`
- `config set dhcp on`
- `config peers add tcp://1.2.3.4:11010`
- `start-network`
- `peer list`（验证只读 RPC 通路）
- `stop-network`
- `exit`

#### 5.3 最小单测（可选）

在不引入复杂依赖的前提下：
- 对 `management_cli::command::parse_line()` 加入单测，覆盖关键命令解析（不启动网络、不触网）。

## Rollout Notes

- 本计划优先保证“可编译 + 可运行 + SSH 内命令闭环”。后续增强项（不在本轮范围）：
  - authorized_keys 文件读取、热更新
  - 更完整的 config key 覆盖（listeners/mapped-listeners/external-node/hostname 等）
  - 多实例与持久化配置
  - 权限模型（只读/可写命令分组、审计日志）

