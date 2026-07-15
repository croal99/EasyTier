# 使用 russh 为 easytier-core 增加最小 SSH Server，并通过内置命令 start/stop 核心网络功能（集成部分 easytier-cli 管理命令）

## Summary

基于 `russh` 在 `easytier-core` 内嵌一个最小可运行的 SSH Server，并将 EasyTier 的“核心网络功能（network instance）”改为按需启动，支持：

* `shell + exec`

* 仅公钥认证

* SSH 启动优先：进程启动后先启动 SSH Server，不自动启动 EasyTier 核心网络功能

* 登录成功后执行内置管理命令，而不是启动外部 `easytier-cli` 进程

* 通过内置命令设置 `network-name`、`network-secret`、自动 IP、peers（节点地址）等

* 通过 `start-network` 启动 EasyTier 核心网络功能（单 instance）

* 通过 `stop-network` 停止 EasyTier 核心网络功能（SSH 仍保持运行）

* 首版命令范围：network 生命周期/配置命令 + easytier-cli 只读管理命令子集

* 默认监听 `localhost`

* 使用硬编码测试公钥作为 PoC 认证入口

* 默认直接编进 `easytier-core`，不额外放到 Cargo feature 后

首版目标是打通一条最小闭环：

1. 启动 `easytier-core`（此时仅启动 SSH，不启动核心网络功能）
2. 内置 SSH Server 在本地监听
3. 用测试公钥登录成功
4. 通过内置命令设置网络参数（network-name/secret/dhcp/peers 等）
5. 执行 `start-network` 启动 EasyTier 核心网络功能
6. 执行只读管理命令查看运行状态（集成的 easytier-cli 子集）
7. 执行 `stop-network` 停止 EasyTier 核心网络功能

## Current State Analysis

### 1. `easytier-core` 启动链路适合挂接新的后台服务，但当前会自动启动核心功能

* 入口二进制在 [easytier/src/easytier-core.rs](file:///Volumes/CODE/CTA/EasyTier/easytier/src/easytier-core.rs)

* 实际主流程在 [easytier/src/core.rs](file:///Volumes/CODE/CTA/EasyTier/easytier/src/core.rs)

* `run_main()` 当前已经在启动时创建：

  * `NetworkInstanceManager`

  * `ApiRpcServer`

  * 可选 `web_client`

现有结构说明：SSH Server 最自然的接入点确实是 `run_main()`；但为了满足“先启动 SSH，核心网络功能不启动”，需要把当前 `run_main()` 中的“自动启动网络实例”和“启动 RPC Portal”改为由 SSH 内置命令触发。

### 2. `easytier-cli` 的核心管理能力已存在，但当前耦合在二进制文件中

* CLI 入口和 clap 命令定义在 [easytier/src/easytier-cli.rs](file:///Volumes/CODE/CTA/EasyTier/easytier/src/easytier-cli.rs)

* `CommandHandler` 定义在该文件内，包含大量可复用查询逻辑

* 现有只读命令能力已经比较完整，核心包括：

  * `handle_peer_list`

  * `handle_route_list`

  * `handle_node`

  * `handle_stats_show`

  * `handle_logger_get`

  * 以及若干 `fetch_*` 方法

当前问题不是“没有管理能力”，而是：

* 命令解析、RPC 调用、数据整形、输出打印都混在一个二进制文件里

* `easytier-core` 无法直接复用这些能力

### 3. CLI 当前通过本地 RPC 管理 core；本次需要将 RPC 与实例启动改为按需

* `easytier-cli` 使用 `StandAloneClient<TcpTunnelConnector>`

* 它通过 `tcp://<rpc_portal>` 访问 `easytier-core` 暴露的 API

* RPC 服务注册点在 [easytier/src/rpc\_service/api.rs](file:///Volumes/CODE/CTA/EasyTier/easytier/src/rpc_service/api.rs)

* `ApiRpcServer::new()` 当前已经负责：

  * 解析 RPC 地址

  * 注册服务

  * 启动 StandAloneServer

这意味着 SSH 集成可以复用现有 RPC/CLI 管理体系，但为了满足“先 SSH 后核心功能”，需要引入按需生命周期：

* 进程启动：只启动 SSH Server

* `start-network`：启动 loopback RPC Portal（`ApiRpcServer`）并启动单个 network instance

* `stop-network`：停止该 network instance，并关闭 loopback RPC Portal（drop server）

这样既能在 network 启动后复用现有 CLI 的只读管理能力，也允许在 network 未启动时通过 SSH 调整配置。

### 4. 已确认的用户偏好 / 需求边界

本次计划按以下已确认选择制定：

* 交互方式：`shell + exec`

* 认证方式：仅公钥认证

* 公钥来源：首版写死测试公钥

* 命令范围：内置网络配置/生命周期命令 + easytier-cli 只读管理命令子集

* shell 形态：带 PTY 的类终端 shell

* 暴露方式：默认监听 `localhost`

* 构建方式：默认编进 `easytier-core`，不额外加 feature gate

* RPC 启动：不在 SSH-only 阶段启动；仅在 `start-network` 后启动，`stop-network` 后关闭

* 实例数量：单个 instance

* 配置持久化：不持久化（仅内存态）

这决定了本次方案是：

* 一个 PoC / 最小落地版

* 重点是“代码路径打通”

* 不以生产级安全配置为目标

## Assumptions & Decisions

### 明确决策

1. 不启动外部 `easytier-cli` 进程\
   采用“共享库化 CLI 管理能力 + SSH 内置命令分发”的方式。

2. network 启动前的配置命令不走 RPC\
   在 network 未启动时，需要一套“修改内存态配置 + 查看配置”的内置命令。

3. network 启动后的只读管理命令复用 RPC/CLI 行为\
   执行 `start-network` 后启动 loopback RPC Portal，使只读管理命令可以通过本地 RPC 查询状态，减少重复实现。

4. shell 与 exec 共用一套命令解析/执行内核\
   `exec` 执行单次命令，shell 使用 PTY channel 循环读取行并回写结果。

5. 首版仅支持：配置/生命周期命令 + 只读管理命令\
   不包含：

   * connector add/remove

   * mapped-listener add/remove

   * port-forward add/remove

   * logger set

   * credential generate/revoke

   * service install/start/stop/uninstall

6. 首版安全边界明确为 PoC\
   硬编码测试公钥只用于最小落地验证，计划中会把该部分集中到一个明显的常量/模块，后续方便替换为配置化来源。

### 允许的首版简化

1. 输出格式只保留文本表格 / 文本摘要\
   不要求首版完整支持 CLI 的 `--json`、`--no-trunc`、`--verbose` 全组合。

2. SSH shell 仅支持单行命令、帮助、退出\
   不做历史记录、补全、复杂终端控制。

3. PTY 只做最小协商\
   接受 PTY 请求、保存基本尺寸信息、按行式 shell 输出提示符；不做复杂 ANSI/全屏终端行为。

## Proposed Changes

### A. `easytier/Cargo.toml`

#### 修改内容

* 增加 `russh`

* 增加 `russh-keys`（如该版本拆分）

* 视 `russh` 版本需要补充 `sha2` / `base64` / `futures` 的配套依赖，但优先复用现有依赖

#### 原因

* 项目当前已使用 Tokio，`russh` 是最适合内嵌式异步 SSH server 的选择

* 用户已明确指定使用 `russh`

#### 实现要求

* 依赖版本选择以当前 Rust 1.95 / Tokio 1.x 兼容为前提

* 不新增 feature gate，默认直接编入 `easytier-core`

***

### B. 新增共享管理命令模块

#### 建议新增文件

* `easytier/src/management_cli/mod.rs`

* `easytier/src/management_cli/command.rs`

* `easytier/src/management_cli/output.rs`

如实现中觉得更合适，也可以用单文件：

* `easytier/src/management_cli.rs`

但建议拆成模块，避免再次把 SSH 与 CLI 逻辑揉进一个超大文件。

#### 修改内容（更新）

在保留“复用 easytier-cli 只读管理能力”的前提下，新增一层统一的**内置命令路由**，支持两类场景：

1. network 未启动：允许通过命令修改内存态配置，并启动/停止 network
2. network 已启动：允许执行 easytier-cli 的只读管理命令子集（通过 loopback RPC 查询）

因此共享模块应至少包含：

* 文本命令解析器（支持配置/生命周期命令 + 只读管理命令）

* 内置命令路由器（根据当前运行状态分发到本地配置逻辑或 RPC 查询逻辑）

* 基于本地 loopback RPC 的只读执行器

* 文本输出渲染器（返回 `String`，不直接 `println!`）

#### 首版命令范围（更新：含 network 配置/生命周期）

首版建议命令分两类：

**A. network 未启动也可用的“配置/生命周期”命令（新增）**

* `config show`

* `config set network-name <name>`

* `config set network-secret <secret>`

* `config set dhcp <on|off>`（“自动 IP”）

* `config set ipv4 <CIDR>|off`

* `config set ipv6 <CIDR>|off`

* `config peers add <url>`

* `config peers remove <url>`

* `config peers clear`

* `start-network`

* `stop-network`

* `status`

**B. network 已启动后可用的“只读管理命令”（来自 easytier-cli 子集）**

* `help`

* `peer list`

* `peer ipv6`

* `peer-center`

* `route list`

* `route dump`

* `node`

* `node info`

* `node config`

* `vpn-portal`

* `proxy`

* `acl stats`

* `port-forward list`

* `whitelist show`

* `stats show`

* `stats prometheus`

* `logger get`

* `credential list`

* `exit`

* `quit`

#### 原因

* 这些命令基本都已存在于 `CommandHandler` 的只读路径里

* 可最大化复用现有 fetch / render 逻辑

* 不触碰配置修改、副作用、系统服务等高风险路径

#### 实现方式（更新）

1. 新增一个公共入口（建议命名），统一处理三类命令：

   * 配置类命令（直接修改内存态 `TomlConfigLoader`）

   * 生命周期类命令（`start-network`/`stop-network`/`status`）

   * 只读管理命令（通过 loopback RPC 复用现有 CLI 查询逻辑）

```rust
pub struct EmbeddedCommandRouter { ... }

impl EmbeddedCommandRouter {
    pub async fn execute_line(&self, line: &str) -> anyhow::Result<CommandResult>;
}
```

其中 `CommandResult` 至少包含：

* 输出文本

* 是否请求退出会话

1. 将 `CommandHandler` 中只读 RPC client 获取和 `fetch_*` 方法迁移到共享模块
2. 将 `print_output` 下沉为“返回 String”，供 SSH/CLI 复用
3. 保持 `easytier-cli.rs` 继续使用 clap 负责外部 CLI 解析，但其只读命令执行尽量复用共享模块，避免两份实现

***

### C. 精简并重接 `easytier/src/easytier-cli.rs`

#### 修改内容

* 保留现有 clap 结构和子命令定义

* 将 `match cli.sub_command` 中对应的只读分支改为调用共享执行模块

* 尽量把现有 `CommandHandler` 迁出或缩减为共享模块的薄包装

#### 原因

* 避免 SSH 和 CLI 后续出现两份实现

* 让 CLI 与 SSH 使用同一套查询逻辑和输出行为

#### 目标结果

`easytier-cli` 仍保持原有使用方式，但内部更多变成：

* clap -> 共享命令模型 / 执行器 -> 字符串输出

而不是：

* clap -> 二进制内私有 handler -> 直接 stdout 打印

***

### D. 新增 SSH Server 模块

#### 建议新增文件

* `easytier/src/ssh_server/mod.rs`

* `easytier/src/ssh_server/server.rs`

* `easytier/src/ssh_server/session.rs`

* `easytier/src/ssh_server/auth.rs`

#### 修改内容

实现一个最小 `russh` server，职责分层如下：

1. `auth.rs`

   * 提供硬编码测试公钥

   * 校验登录公钥

   * 未来替换成配置化来源时，改动集中在这里

2. `server.rs`

   * 负责监听 `127.0.0.1:<port>`

   * 创建 `russh` 配置

   * 生成 / 加载服务端 host key

   * 启动 server task

3. `session.rs`

   * 处理 channel 生命周期

   * 处理 shell / exec / PTY 请求

   * 调用共享 `EmbeddedCommandRouter`

   * 回写输出与提示符

#### 首版行为定义

##### 认证

* 仅接受硬编码测试公钥

* 不支持密码认证

##### shell

* 仅在登录后请求 `shell` 时进入

* 支持 PTY request，但交互仍按“行式 shell”处理

* 显示固定提示符，例如：

```text
easytier> 
```

* 支持：

  * `help`

  * `config ...` / `start-network` / `stop-network` / `status`

  * 内置只读管理命令

  * `exit`

  * `quit`

##### exec

* 支持 `ssh ... "peer list"` 这类单次命令

* 单次执行后写回输出并关闭 channel

##### 错误处理

* 未知命令返回明确提示

* RPC 不可达 / 查询失败时返回错误文本，但不导致整个 SSH 服务退出

#### 关键实现决策（更新：先 SSH，network 按需）

命令后端分两段：

* network 未启动：仅允许 `config/*`、`start-network`、`status` 等本地命令；不走 RPC

* network 已启动：只读管理命令通过 loopback RPC 访问 `ApiRpcServer`（由 `start-network` 启动）

因此 SSH session 只依赖共享 `EmbeddedCommandRouter`，而不直接依赖 `run_main()` 的启动顺序。

***

### E. 调整 `easytier/src/rpc_service/api.rs`（更新：按需启动 + loopback 绑定）

#### 修改内容

为 `ApiRpcServer` 增加“暴露实际 RPC 地址”的能力，至少满足其中一种：

1. `ApiRpcServer::new(...) -> (ApiRpcServer<_>, SocketAddr)`
2. 在 `ApiRpcServer` 结构体中保存 `rpc_addr` 并提供 getter
3. 把 `parse_rpc_portal` 公开化，由 `core.rs` 先得到地址再传入

#### 原因

内嵌 SSH server 在 `start-network` 后需要知道 RPC 端口，才能通过本地 RPC 复用现有管理逻辑。

#### 推荐方式

优先选择：

* `ApiRpcServer::new(...)` 内部保留 `rpc_addr`

* 提供 `rpc_addr()` getter

这样对现有调用点改动最小。

另外由于本次要求 RPC 不在 SSH-only 阶段启动，且 SSH 默认只监听 `localhost`，计划将 RPC Portal 也限制在 loopback：

* `start-network` 创建 `TcpTunnelListener` 时使用 `tcp://127.0.0.1:<port>`

* 端口选择复用 `find_free_tcp_port(15888..15900)`

***

### F. 调整 `easytier/src/core.rs`（更新：默认先 SSH，network 按需 start/stop）

#### 修改内容

在 `Cli` 中增加最小 SSH 配置参数，建议新增一个 `SshServerOptions`，风格参考现有 `RpcPortalOptions`。

#### 建议参数

* `--ssh-server`

  * 首版默认值：`true`

  * 作用：是否启用内置 SSH server

* `--ssh-listen`

  * 首版默认值：`127.0.0.1:2222`

  * 作用：SSH 监听地址

由于用户要求“默认监听 localhost”，因此默认值应直接启用本地监听。

#### 启动链路调整

将现有 `run_main()` 的“自动启动网络实例 + 启动 RPC Portal”拆成按需控制：

* 进程启动（SSH-only）：只启动 SSH Server，并初始化一份内存态配置（`TomlConfigLoader::default()`）

* `start-network`：按需创建 `NetworkInstanceManager`，启动 loopback `ApiRpcServer`，并启动单个 network instance

* `stop-network`：删除该 instance（触发 launcher drop 停止），并 drop `ApiRpcServer`

为此在 `core.rs` 中新增一个全局控制器状态（建议命名 `CoreRuntimeController`），由 SSH 内置命令调用：

* `config set ...` / `config peers ...`：修改内存态 config

* `start-network`：启动 manager + RPC + instance

* `stop-network`：停止 instance + RPC（保留 config 以便再次 start）

* `status`：输出当前 config 摘要、是否已启动、RPC 端口、instance id 等

#### 原因

* `core.rs` 是唯一合适的配置入口

* 这里也是 SSH server 与 core 生命周期绑定的最佳位置

***

### G. 调整 `easytier/src/lib.rs`

#### 修改内容

新增模块导出：

* `mod ssh_server;`

* `mod management_cli;`

如需要给二进制共享使用，则暴露为：

* `pub mod ssh_server;`

* `pub mod management_cli;`

#### 原因

让 `easytier-core.rs`、`easytier-cli.rs` 与共享模块能以统一方式引用。

## Implementation Steps

1. 在 `Cargo.toml` 中引入 `russh` 相关依赖
2. 新建 `ssh_server` 模块（仅负责 SSH 协议层）
3. 新建 `management_cli` 共享模块（命令路由 + RPC 只读命令复用 + 输出渲染）
4. 实现 `CoreRuntimeController`（管理内存态 config + start/stop network + start/stop RPC）
5. 将 `easytier-cli.rs` 的只读命令执行路径迁移/复用到 `management_cli`
6. 在 `core.rs` 中：

   * 进程启动先启动 SSH（默认 localhost）

   * 不自动启动 network instance / RPC
7. 实现内置命令：

   * `config ...`

   * `start-network`

   * `stop-network`

   * `status`
8. `start-network` 时启动 loopback RPC Portal，并将地址注入只读命令执行器
9. 增加最小测试（命令解析 + start/stop 状态机 + SSH exec 基础路径）
10. 编译并做交互验证

## Testing + Acceptance Criteria

### 编译验证

至少验证：

```bash
cargo +1.95 build -p easytier --bin easytier-core
cargo +1.95 build -p easytier --bin easytier-cli
```

### 单元 / 集成测试建议

#### 1. 共享命令解析测试

新增测试覆盖：

* `help`

* `peer list`

* `route list`

* `node`

* `stats show`

* `logger get`

* `exit`

* 非法命令

目标：

* 字符串命令可以被正确解析为内部命令模型

* 非法输入返回稳定错误信息

#### 2. 共享输出渲染测试

对 `print_output` 下沉后的文本渲染做快照或结构测试，重点覆盖：

* 空结果

* 普通表格

* 关键字段顺序

#### 3. SSH 认证测试

至少覆盖：

* 测试公钥可登录

* 非测试公钥被拒绝

#### 4. SSH 命令路径测试

理想情况下新增最小 Tokio 集成测试，验证：

* `exec "help"` 返回成功

* `exec "status"` / `exec "start-network"` / `exec "stop-network"` 能走通并返回文本

* `shell` 登录后收到提示符

### 手工验收

1. 启动 `easytier-core`（此时仅启动 SSH，不启动核心网络功能）
2. 确认本地监听 `127.0.0.1:2222`
3. 使用测试私钥登录
4. 先设置配置并启动 network：

```bash
ssh -p 2222 user@127.0.0.1
```

进入后可执行：

```text
status
config show
config set network-name testnet
config set network-secret testsecret
config set dhcp on
config peers add tcp://1.2.3.4:11010
start-network
help
peer list
route list
node
stats show
logger get
stop-network
exit
```

1. 验证 exec：

```bash
ssh -p 2222 user@127.0.0.1 "status"
ssh -p 2222 user@127.0.0.1 "config set network-name testnet"
ssh -p 2222 user@127.0.0.1 "start-network"
ssh -p 2222 user@127.0.0.1 "peer list"
ssh -p 2222 user@127.0.0.1 "stop-network"
```

应返回文本结果并正常退出

## Out of Scope

本次计划明确不包含：

* 生产级 authorized\_keys 配置

* 密码认证

* 多用户 / 多角色

* 命令修改能力

* SFTP / SCP

* 远程端口转发 / 本地端口转发

* 命令历史 / 自动补全 / 复杂 ANSI 终端

* 审计日志与命令权限系统

* feature-gate / 体积优化

* 多实例 start/stop

* 配置持久化到磁盘

## Risks

1. `russh` 的 PTY / shell 事件模型与预期存在细节差异\
   处理方式：shell 语义尽量保持行式，避免追求完整终端行为。

2. `easytier-cli.rs` 迁移时容易影响原有 CLI 行为\
   处理方式：优先迁只读命令，共享层返回字符串，CLI 继续保持 clap 外壳不变。

3. 通过本地 RPC 复用命令时，需要拿到准确 RPC 地址\
   处理方式：`start-network` 明确创建 loopback RPC listener，并把地址写入 controller 状态供命令执行器使用。

4. 硬编码测试公钥具备明显安全风险\
   处理方式：仅作为 PoC，代码中要集中放置并用注释标明后续必须配置化。

## Verification Steps

计划完成后，执行阶段应按以下顺序验证：

1. 先跑 `cargo fmt`
2. 编译 `easytier-core`
3. 编译 `easytier-cli`
4. 跑共享命令解析 / 输出渲染测试
5. 跑 SSH 认证与命令路径测试
6. 本地手工启动 core
7. 用测试密钥验证：配置命令 -> start-network -> 只读命令 -> stop-network
8. 检查 `easytier-cli` 原有只读命令未回归

