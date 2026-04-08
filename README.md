# TPN Worker — 部署与运维文档

TPN (TAO Private Network) Worker 是 Bittensor Subnet 65 的工作节点，为去中心化 VPN 网络提供 WireGuard、SOCKS5 和 HTTP CONNECT 代理服务。

重构后的 Worker 是一个 **单一 Rust 二进制 + 一个 SQLite 文件**，零外部进程依赖。

## 架构概览

```
┌─────────────────────────────────────────────────────┐
│                   tpn-worker 二进制                    │
│                                                     │
│  ┌──────────┐  ┌──────────┐  ┌───────────────────┐  │
│  │ HTTP API │  │ SOCKS5   │  │ HTTP CONNECT      │  │
│  │ :3000    │  │ :1080    │  │ :3128             │  │
│  └──────────┘  └──────────┘  └───────────────────┘  │
│  ┌──────────────────┐  ┌──────────────────────────┐  │
│  │ WireGuard Server │  │ SQLite (嵌入式)           │  │
│  │ :51820/udp       │  │ tpn-worker.db            │  │
│  └──────────────────┘  └──────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

**监听端口：**

| 端口 | 协议 | 服务 | 对外开放 |
|------|------|------|----------|
| 3000 | TCP | HTTP API（健康检查、lease 管理、统计） | 是 — 矿池需要访问此端口进行 lease 分配和健康检查 |
| 1080 | TCP | SOCKS5 代理 | 是 — VPN 客户端通过此端口连接代理 |
| 3128 | TCP | HTTP CONNECT 代理 | 是 — VPN 客户端通过此端口连接代理 |
| 51820 | UDP | WireGuard VPN | 是 — VPN 客户端通过此端口建立 WireGuard 隧道 |

---

## 系统要求

- **操作系统：** Linux（需要内核 WireGuard 支持）
- **内核版本：** >= 5.6（原生 WireGuard）或已加载 `wireguard` 内核模块
- **权限：** 必须以 `root` 运行（创建 WireGuard 网络接口需要 `CAP_NET_ADMIN`）
- **设备：** `/dev/net/tun` 可用
- **系统工具：** `wireguard-tools`（提供 `wg` 命令）、`iproute2`（提供 `ip` 命令）
- **磁盘：** < 100 MB（二进制 ~30 MB + SQLite 数据库）
- **内存：** < 64 MB（典型运行时）
- **网络：** 公网 IP，防火墙放行 TCP 3000/1080/3128 和 UDP 51820

---

## 部署

### 1. 准备运行环境

```bash
# 加载 WireGuard 内核模块（如果内核版本 < 5.6）
sudo modprobe wireguard

# 确认 TUN 设备存在
ls -l /dev/net/tun

# 安装运行时工具
apt-get install -y wireguard-tools iproute2 curl
```

### 2. 安装二进制

```bash
wget https://github.com/${{ github.repository }}/releases/download/latest/tpn-worker-linux-ubuntu22-amd64 -O /usr/local/bin/tpn-worker && chmod +x /usr/local/bin/tpn-worker
```

### 3. 初始化配置

首次运行任意命令时自动创建配置目录和默认 `.env`：

```bash
tpn-worker config
```

输出：

```
Generated default config: /root/.config/tpn-worker/.env
# Config file: /root/.config/tpn-worker/.env

# TPN Worker 配置文件
# ...
```

所有数据存放在 `$HOME/.config/tpn-worker/` 下：

```
$HOME/.config/tpn-worker/
├── .env              # 环境配置文件
└── tpn-worker.db     # SQLite 数据库（运行后自动创建）
```

### 4. 编辑配置

```bash
vim ~/.config/tpn-worker/.env
```

至少填写以下必填项：

```bash
SERVER_PUBLIC_HOST=
MINING_POOL_URL=http://pool.example.com:3000
PAYMENT_ADDRESS_EVM=0xYourEvmAddress
PAYMENT_ADDRESS_BITTENSOR=5YourBittensorAddress
```

查看当前配置：

```bash
tpn-worker config
```

### 5. 直接运行

```bash
sudo tpn-worker
```

程序启动时自动从 `~/.config/tpn-worker/.env` 加载配置，无需手动 `source` 或 `env` 命令。

### 6. 使用 systemd 管理（推荐）

创建 `/etc/systemd/system/tpn-worker.service`：

```ini
[Unit]
Description=TPN Worker Node
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=/usr/local/bin/tpn-worker
Restart=always
RestartSec=10

# 安全加固
LimitNOFILE=65535
ProtectSystem=strict
ReadWritePaths=/root/.config/tpn-worker
PrivateTmp=true

# WireGuard 所需权限
AmbientCapabilities=CAP_NET_ADMIN CAP_SYS_MODULE
CapabilityBoundingSet=CAP_NET_ADMIN CAP_SYS_MODULE

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable tpn-worker
sudo systemctl start tpn-worker

# 查看状态
sudo systemctl status tpn-worker

# 查看日志
journalctl -u tpn-worker -f
```

### 7. 验证运行

```bash
# 健康检查
curl http://localhost:3000/
# → {"status":"ok","uptime_seconds":5,"mode":"worker","version":"0.1.0",...}

# 统计信息
curl http://localhost:3000/api/stats

# WireGuard 接口状态
wg show wg0

# 测试 WireGuard lease 分配
curl "http://localhost:3000/api/lease/new?type=wireguard&lease_seconds=60&format=json"

# 测试 SOCKS5 lease 分配
curl "http://localhost:3000/api/lease/new?type=socks5&lease_seconds=60&format=json"

# 确认数据库已创建
ls -lh ~/.config/tpn-worker/tpn-worker.db
sqlite3 ~/.config/tpn-worker/tpn-worker.db ".tables"
# → timestamps  worker_registration_log  worker_socks5_configs  worker_wg_server  worker_wireguard_configs

# 查看 Web Dashboard
# 浏览器打开 http://localhost:3000/dashboard
```

---

## 环境变量

### 必填

| 变量 | 说明 | 示例 |
|------|------|------|
| `SERVER_PUBLIC_HOST` | 本节点公网 IP 或域名 | `203.0.113.1` |
| `MINING_POOL_URL` | 矿池注册地址 | `http://pool.example.com:3000` |

### 收款地址

这两个地址在向矿池注册时提交，矿池据此发放奖励。技术上可以留空（不影响启动），但**不填就收不到奖励**。

| 变量 | 说明 | 示例 |
|------|------|------|
| `PAYMENT_ADDRESS_EVM` | EVM 链收款地址 | `0xABC...DEF` |
| `PAYMENT_ADDRESS_BITTENSOR` | Bittensor SS58 收款地址 | `5HBq...xyz` |

### 服务器

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `RUN_MODE` | `worker` | 运行模式：`worker` / `validator` / `miner` |
| `SERVER_PUBLIC_PORT` | `3000` | HTTP API 监听端口 |
| `SERVER_PUBLIC_PROTOCOL` | `http` | 公开协议（`http` 或 `https`） |
| `SERVER_PUBLIC_URL` | *(空)* | 完整公开 URL，设置后覆盖 protocol+host+port 拼接 |
| `LOG_LEVEL` | `info` | 日志级别：`trace` / `debug` / `info` / `warn` / `error` |

### WireGuard

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `WIREGUARD_PEER_COUNT` | `253` | 最大 peer 数（1-253，受 /24 子网限制） |
| `WIREGUARD_SERVERPORT` | `51820` | WireGuard UDP 监听端口 |
| `WG_INTERFACE_NAME` | `wg0` | 网络接口名称 |
| `WG_SUBNET` | `10.13.13.0` | 子网基地址（.1 为服务端，peer 从 .2 起） |
| `WG_DNS` | `1.1.1.1` | 下发给客户端的 DNS |

### 代理

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `SOCKS5_PORT` | `1080` | SOCKS5 代理端口 |
| `HTTP_PROXY_PORT` | `3128` | HTTP CONNECT 代理端口 |
| `PROXY_CREDENTIAL_COUNT` | `256` | 自动生成的代理凭证数量 |
| `PRIORITY_SLOTS` | `5` | 优先级 lease 保留槽位 |

### 数据库

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `SQLITE_PATH` | `~/.config/tpn-worker/tpn-worker.db` | SQLite 文件路径（不存在时自动创建） |

### 安全

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `LEASE_TOKEN_SECRET` | *(每次启动随机生成)* | Lease 延期 HMAC 签名密钥。固定此值可使 token 跨重启有效 |
| `ADMIN_API_KEY` | *(空)* | 管理接口 API 密钥 |

### 后台任务

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DAEMON_INTERVAL_SECONDS` | `300` | 清理/注册循环间隔（秒） |

### 开发 / 调试

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `CI_MODE` | `false` | 启动时清空所有数据表 |
| `CI_MOCK_WG_CONTAINER` | `false` | 跳过 WireGuard 启动，返回 mock 配置 |
| `CI_MOCK_WORKER_RESPONSES` | `false` | 跳过矿池 IP 校验 |
| `FORCE_DESTROY_DATABASE` | `false` | 启动时强制重建所有表 |

---

## API 接口

### `GET /` — 健康检查

```json
{
  "notice": "I am a TPN Network worker component running v1.3.3",
  "info": "https://tpn.taofu.xyz",
  "version": "1.3.3",
  "last_start": "2026-04-07T10:00:00+00:00",
  "branch": "main",
  "hash": "abc1234",
  "MINING_POOL_URL": "http://pool.example.com:3000",
  "SERVER_PUBLIC_HOST": "203.0.113.1",
  "SERVER_PUBLIC_PORT": "3000",
  "SERVER_PUBLIC_PROTOCOL": "http"
}
```

### `GET /ping` — 存活探测

返回纯文本 `pong`。

### `GET /api/lease/new` — 申请 / 延期 Lease

#### 新建 Lease

| 参数 | 必填 | 说明 |
|------|------|------|
| `type` | 否 | `wireguard`（默认）或 `socks5` |
| `lease_seconds` | 是 | Lease 时长（秒） |
| `format` | 否 | `json`（默认）或 `text` |
| `priority` | 否 | `true` 使用优先级槽位 |

#### 延期 Lease

| 参数 | 必填 | 说明 |
|------|------|------|
| `extend_ref` | 是 | 原响应的 `X-Lease-Ref` 值 |
| `extend_expires_at` | 是 | 原响应的 `X-Lease-Expires` 值（乐观锁校验） |
| `lease_token` | 是 | 原响应的 `X-Lease-Token` 值（HMAC 签名校验） |
| `lease_seconds` | 是 | 延期时长（秒） |

#### 响应头

| Header | 说明 |
|--------|------|
| `X-Lease-Ref` | Lease 标识（WG peer ID 或 SOCKS5 用户名） |
| `X-Lease-Expires` | 过期时间戳（毫秒） |
| `X-Lease-Token` | HMAC 签名 token（延期时回传） |

#### WireGuard 响应示例

```json
{
  "interface": {
    "Address": "10.13.13.2/32",
    "PrivateKey": "base64...",
    "DNS": "1.1.1.1"
  },
  "peer": {
    "PublicKey": "base64...",
    "PresharedKey": "base64...",
    "AllowedIPs": "0.0.0.0/0, ::/0",
    "Endpoint": "203.0.113.1:51820"
  }
}
```

#### SOCKS5 响应示例

```json
{
  "username": "u_a1b2c3d4",
  "password": "p_LongRandomPassword...",
  "ip_address": "203.0.113.1",
  "port": 1080
}
```

### `GET /api/stats` — 运行统计

```json
{
  "status": "ok",
  "mode": "worker",
  "uptime_seconds": 3600,
  "version": "0.1.0",
  "wireguard": {
    "peer_count": 253,
    "active_leases": 12
  },
  "socks5": {
    "priority_slots": 5,
    "available_non_priority": 230
  }
}
```

### `GET /api/config/new`

`/api/lease/new` 的兼容别名。

### `GET /dashboard` — Web 状态页

内嵌暗色主题 Web 页面，实时展示 Worker 运行状态，每 5 秒自动刷新。

展示内容：
- **Worker 信息** — 版本、运行模式、运行时间、Git 信息
- **Mining Pool** — 连接状态、最后注册时间、矿池 URL
- **WireGuard** — 活跃 peers/leases、服务器公钥、监听端口、子网
- **代理** — SOCKS5/HTTP 端口、凭证总数/活跃数、可用数
- **网络配置** — 公网地址、端口、协议、Base URL
- **收款地址** — EVM、Bittensor
- **数据库** — 文件路径
- **注册历史** — 最近 50 条记录（时间、状态、HTTP 码、矿池 URL、响应摘要）

### `GET /api/dashboard` — Dashboard JSON 数据

返回完整的 Dashboard 聚合数据，供 Web 页面和外部监控工具使用：

```json
{
  "worker": { "version", "mode", "uptime_seconds", "start_time", "git_branch", "git_hash" },
  "mining_pool": { "url", "name", "last_registration_success", "last_registration_time", "rewards_url", "website_url" },
  "wireguard": { "enabled", "interface", "max_peers", "active_peers", "active_leases", "listen_port", "subnet", "dns", "server_public_key" },
  "proxy": { "socks5_port", "http_proxy_port", "credential_count", "active_credentials", "available_non_priority", "priority_slots" },
  "network": { "public_host", "public_port", "protocol", "base_url" },
  "payment": { "evm_address", "bittensor_address" },
  "database": { "path" },
  "registration_history": [{ "id", "mining_pool_url", "success", "status_code", "response_body", "error_message", "created_at" }]
}
```

---

## 数据库

嵌入式 SQLite，启动时自动创建表，无需手动初始化。

### 数据表

| 表 | 说明 |
|----|------|
| `worker_wireguard_configs` | WireGuard peer lease（id、过期时间、密钥对、allowed_ip） |
| `worker_wg_server` | WireGuard 服务器密钥对（重启后自动恢复，保持公钥不变） |
| `worker_socks5_configs` | SOCKS5 凭证（用户名、密码、可用状态、过期时间） |
| `worker_registration_log` | 矿池注册历史（成功/失败、HTTP 状态码、响应体），保留最近 200 条 |
| `timestamps` | 通用键值时间戳 |

### 运行时参数

- **WAL 模式：** 支持并发读取
- **busy_timeout = 5000ms：** 写入冲突自动等待重试
- **foreign_keys = ON**

### 备份

```bash
# 在线热备份（推荐）
sqlite3 ~/.config/tpn-worker/tpn-worker.db ".backup /path/to/backup.db"

# 或直接复制（需确保 worker 未运行）
cp ~/.config/tpn-worker/tpn-worker.db ~/tpn-worker-backup.db
```

### 常用查询

```bash
# 活跃 WireGuard peer 数量
sqlite3 ~/.config/tpn-worker/tpn-worker.db "SELECT COUNT(*) FROM worker_wireguard_configs WHERE expires_at > $(date +%s)000;"

# 可用 SOCKS5 凭证数量
sqlite3 ~/.config/tpn-worker/tpn-worker.db "SELECT COUNT(*) FROM worker_socks5_configs WHERE available = 1;"

# WireGuard 服务器公钥（客户端可验证）
sqlite3 ~/.config/tpn-worker/tpn-worker.db "SELECT public_key FROM worker_wg_server ORDER BY id DESC LIMIT 1;"
```

---

## 运行生命周期

### 启动流程

```
1. 解析环境变量 / CLI 参数
2. 生成 LEASE_TOKEN_SECRET（如未配置）
3. 初始化日志
4. 连接 SQLite（自动创建文件，启用 WAL）
5. 创建数据表（IF NOT EXISTS，幂等）
6. 启动 WireGuard 服务器
   ├── 从 DB 加载 / 生成服务器密钥对
   ├── 创建内核接口 wg0
   └── 从 DB 恢复未过期 peer
7. 初始化 SOCKS5 凭证（从 DB 加载 / 自动生成 256 组）
8. 启动 SOCKS5 代理（:1080）
9. 启动 HTTP CONNECT 代理（:3128）
10. 启动 HTTP API（:3000）
11. 向矿池注册（含 WG + SOCKS5 配置探针）
12. 进入后台循环（清理 + 重注册）
```

### 后台任务（每 DAEMON_INTERVAL_SECONDS）

1. 查询过期 WireGuard peer ID 列表
2. 逐个从内核 WireGuard 接口移除（**防止过期连接继续使用**）
3. 从 DB 删除过期 lease 记录（90 分钟宽限期）
4. 清理过期时间戳（1 年宽限期）
5. 重新加载 SOCKS5 凭证到内存
6. 向矿池重新注册

### 优雅关闭（SIGTERM / Ctrl+C）

1. 停止接受新 HTTP 请求
2. 取消 SOCKS5 和 HTTP CONNECT 监听
3. 销毁 WireGuard 内核接口
4. 退出进程（SQLite 自动关闭）

### 重启恢复

Worker 设计为可安全重启，数据不丢失：

- **WireGuard 服务器密钥：** 从 `worker_wg_server` 表恢复，公钥不变，已发放的客户端配置在 lease 有效期内仍可使用
- **活跃 WireGuard peer：** 从 `worker_wireguard_configs` 表恢复所有未过期 peer 到内核接口
- **SOCKS5 凭证：** 从 `worker_socks5_configs` 表全量加载到内存
- **Lease token：** 如果 `LEASE_TOKEN_SECRET` 是固定值，旧 token 跨重启有效；如果是随机生成，重启后旧 token 失效（进行中的 lease 延期会被拒绝，但 lease 本身在过期前仍可使用）

---

## 安全机制

### Lease Token 认证

防止第三方伪造 lease 延期请求：

1. 新建 lease → 服务端用 `LEASE_TOKEN_SECRET` 对 `lease_ref` 做 HMAC-SHA256 签名 → 返回 `X-Lease-Token`
2. 延期 lease → 客户端回传 token → 服务端校验签名
3. 校验使用常量时间比较（`subtle::ConstantTimeEq`），防止时序攻击

### 代理凭证

- SOCKS5 和 HTTP CONNECT 均要求用户名+密码认证
- 密码校验使用常量时间比较
- 凭证按 lease 生命周期自动回收
- 密码格式：`p_` + 32 位随机字母数字（约 190 bit 熵）

### Worker 访问控制

Worker 模式下，`/api/lease/new` 仅接受来自矿池 IP 的请求。验证流程：DNS 解析 `MINING_POOL_URL` → 比对请求源 IP → 不匹配则返回 401。

---

## 源码结构

```
tpn-worker/
├── Cargo.toml              # 依赖清单
├── build.sh                # 构建脚本
├── src/
│   ├── main.rs             # 入口：初始化、启动所有服务、优雅关闭
│   ├── config.rs           # 环境变量 / CLI 配置定义
│   ├── error.rs            # 统一错误类型 → HTTP 状态码映射
│   ├── dashboard.html      # 内嵌 Web 状态页（暗色主题，5s 自动刷新）
│   ├── api/
│   │   ├── health.rs       # GET / 和 GET /ping
│   │   ├── lease.rs        # GET /api/lease/new（核心 lease 管理 + token 校验）
│   │   ├── status.rs       # GET /api/stats
│   │   └── dashboard.rs    # GET /dashboard（HTML）+ GET /api/dashboard（JSON）
│   ├── crypto/
│   │   └── lease_token.rs  # HMAC-SHA256 lease token 签发与校验
│   ├── db/
│   │   ├── pool.rs         # SQLite 连接池（WAL 模式）
│   │   ├── init.rs         # 建表语句（幂等）
│   │   ├── wireguard.rs    # WireGuard lease CRUD
│   │   ├── socks5.rs       # SOCKS5 凭证 CRUD
│   │   ├── cleanup.rs      # 过期数据清理 + WG 接口同步
│   │   ├── timestamps.rs   # 通用时间戳存储
│   │   └── registration_log.rs # 矿池注册历史记录
│   ├── net/
│   │   ├── dns.rs          # DNS 解析（矿池 IP 校验）
│   │   └── ip.rs           # IPv4 工具函数
│   ├── proxy/
│   │   ├── credentials.rs  # 凭证管理（DashMap 缓存 + DB 持久化）
│   │   ├── socks5.rs       # SOCKS5 服务器
│   │   └── http_connect.rs # HTTP CONNECT 代理
│   ├── service/
│   │   ├── lease_manager.rs# Lease 分配与延期业务逻辑
│   │   ├── mining_pool.rs  # 矿池注册
│   │   └── cache.rs        # TTL 缓存
│   ├── sync/
│   │   └── locks.rs        # NamedLockManager（应用级互斥锁）
│   └── wireguard/
│       ├── keygen.rs       # x25519 密钥生成
│       ├── server.rs       # WireGuard 内核接口管理
│       └── peer_manager.rs # Peer 生命周期（分配、释放、回滚）
```

---

## 故障排查

### WireGuard 接口创建失败

```
Failed to create WG interface: ...
```

```bash
# 确认内核模块
lsmod | grep wireguard
sudo modprobe wireguard    # 手动加载

# 确认 TUN 设备
ls -l /dev/net/tun

# 确认以 root 运行
whoami
```

### 数据库锁定

```
database is locked
```

同一 `SQLITE_PATH` 只能运行一个 tpn-worker 实例。检查是否有残留进程：

```bash
fuser ~/.config/tpn-worker/tpn-worker.db
```

### Lease 槽位耗尽

```
All WireGuard peer slots (1-253) exhausted
```

```bash
# 查看当前占用
sqlite3 ~/.config/tpn-worker/tpn-worker.db "SELECT id, datetime(expires_at/1000, 'unixepoch') as expires FROM worker_wireguard_configs ORDER BY expires_at;"

# 手动触发清理（等待下一个 daemon 周期，或重启 worker）
```

### 矿池注册失败

```
Mining pool registration failed: ...
```

```bash
# 测试矿池连通性
curl -v $MINING_POOL_URL

# 确认 SERVER_PUBLIC_HOST 是矿池可达的公网 IP
curl ifconfig.me
```

### 端口冲突

```
Failed to bind HTTP listener
```

```bash
# 查看端口占用
ss -tlnp | grep -E '3000|1080|3128'
ss -ulnp | grep 51820
```
