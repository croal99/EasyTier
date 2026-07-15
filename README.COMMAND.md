# EasyTier SSH Server Command Guide

本文档整理当前 `easytier-core` 内置 SSH server 支持的命令，面向通过 SSH 登录后执行的内置命令行。

注意：
- 当前命令集合以仓库现状为准。
- SSH server 启动后，EasyTier 核心网络默认**不会自动启动**。
- 需要先通过 `config ...` 设置参数，再执行 `start-network` 启动核心网络功能。

## Quick Workflow

典型使用顺序如下：

```text
1. SSH 登录
2. config show
3. config set network-name <name>
4. config set network-secret <secret>
5. config set dhcp on
   或
   config set ipv4 <CIDR>
6. config peers add <url>
7. start-network
8. peer list / node info / route list 等只读管理命令
9. stop-network
10. exit
```

## Base Commands

这些命令在网络未启动时也可以使用：

```text
help
status
start-network
stop-network
exit
quit
```

说明：
- `help`：显示命令帮助。
- `status`：显示当前运行状态和内存中的配置。
- `start-network`：启动 EasyTier 核心网络功能，并启动 loopback RPC portal。
- `stop-network`：停止 EasyTier 核心网络功能，但 SSH server 保持运行。
- `exit` / `quit`：退出当前 SSH shell。

## Config Commands

这些命令用于修改当前内存中的配置：

```text
config show
config set network-name <name>
config set network-secret <secret>
config set dhcp <on|off>
config set ipv4 <CIDR>|off
config set ipv6 <CIDR>|off
config peers add <url>
config peers remove <url>
config peers clear
```

说明：
- `config show`：查看当前内存中的配置。
- `config set network-name <name>`：设置网络名。
- `config set network-secret <secret>`：设置网络密钥。
- `config set dhcp <on|off>`：开启或关闭自动 IP。
- `config set ipv4 <CIDR>|off`：设置静态 IPv4，或关闭 IPv4。
- `config set ipv6 <CIDR>|off`：设置静态 IPv6，或关闭 IPv6。
- `config peers add <url>`：添加一个节点地址。
- `config peers remove <url>`：删除一个节点地址。
- `config peers clear`：清空全部节点地址。

## Read-Only Management Commands

以下命令必须在执行 `start-network` 后才能使用，否则会失败：

```text
peer list
peer ipv6
peer-center
route list
route dump
node
node info
node config
vpn-portal
proxy
acl stats
port-forward list
whitelist show
stats show
stats prometheus
logger get
credential list
```

说明：
- `peer list`：查看节点列表。
- `peer ipv6`：查看公网 IPv6 信息。
- `peer-center`：查看全局 peer map。
- `route list`：查看路由列表。
- `route dump`：导出路由信息。
- `node` / `node info`：查看节点信息。
- `node config`：查看运行中节点配置。
- `vpn-portal`：查看 VPN portal 信息。
- `proxy`：查看代理条目。
- `acl stats`：查看 ACL 统计。
- `port-forward list`：查看端口转发配置。
- `whitelist show`：查看白名单。
- `stats show`：查看统计信息。
- `stats prometheus`：输出 Prometheus 格式统计。
- `logger get`：查看 logger 配置。
- `credential list`：查看 credential 列表。

## Example Session

```text
help
config show
config set network-name demo
config set network-secret 123456
config set dhcp on
config peers add tcp://1.2.3.4:11010
start-network
status
peer list
node info
route list
stop-network
exit
```

## Current Limitations

- 当前只支持单实例运行控制。
- 当前配置为内存态，不会自动持久化到文件。
- 只读管理命令依赖 `start-network` 后启动的 loopback RPC。
- 当前 SSH 认证仍是最小 PoC 方案，公钥来源是代码内置测试 key。
