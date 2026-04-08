# TPN Worker 节点 Rust 重构可行性分析

> **分析版本**: 基于 TPN v1.3.3 | **分析日期**: 2026-04-07

---

## 目录

1. [执行摘要](#1-执行摘要)
2. [Worker 节点现状拆解](#2-worker-节点现状拆解)
3. [逐组件 Rust 替代方案分析](#3-逐组件-rust-替代方案分析)
4. [关键技术挑战](#4-关键技术挑战)
5. [Rust 生态库选型](#5-rust-生态库选型)
6. [架构设计建议](#6-架构设计建议)
7. [与现有系统的兼容性](#7-与现有系统的兼容性)
8. [工作量估算](#8-工作量估算)
9. [收益分析](#9-收益分析)
10. [风险评估](#10-风险评估)
11. [推荐实施路径](#11-推荐实施路径)
12. [结论](#12-结论)

---

## 1. 执行摘要

### 总体结论：可行，但需分阶段实施

Worker 节点的 Rust 重构**技术上完全可行**，且能带来显著收益（内存降低约 10 倍、启动时间降低约 20 倍、单二进制部署）。但存在以下核心约束：

| 维度 | 评估 |
|------|------|
| **技术可行性** | ✅ 高 — Rust 生态已覆盖所有所需能力 |
| **收益** | ✅ 高 — 资源占用、部署复杂度、安全性均有质的提升 |
| **工作量** | ⚠️ 中高 — 预估 2-3 人月（含测试） |
| **最大风险** | ⚠️ Linux 内核交互（WireGuard/netns/iptables）的正确性验证 |
| **推荐策略** | 分 3 阶段：HTTP API → VPN 层 → 代理层，逐步替换 |

---

## 2. Worker 节点现状拆解

### 2.1 当前 Worker 由 4 个独立进程/容器组成

```
┌─ Worker 节点 ─────────────────────────────────────────────┐
│                                                            │
│  ① Node.js 服务 (tpn-federated)                           │
│     ├─ Express HTTP 服务器 (端口 3000)                     │
│     ├─ PostgreSQL 客户端                                   │
│     ├─ 租约管理（WireGuard + SOCKS5）                      │
│     ├─ 矿池注册守护进程                                    │
│     ├─ Docker exec 调用（操控 WireGuard 容器）              │
│     └─ 文件系统监听（密码文件、配置文件）                    │
│                                                            │
│  ② WireGuard 容器 (taofuprotocol/wireguard)               │
│     ├─ WireGuard 内核模块 (wg0 接口)                       │
│     ├─ 最多 253 个 peer 配置                               │
│     └─ 端口 51820/UDP                                      │
│                                                            │
│  ③ Dante 容器 (taofuprotocol/dante)                       │
│     ├─ SOCKS5 代理服务器                                   │
│     ├─ 密码文件热加载                                       │
│     └─ 端口 1080                                           │
│                                                            │
│  ④ 3proxy 容器 (taofuprotocol/tpn-3proxy)                 │
│     ├─ HTTP CONNECT 代理                                   │
│     ├─ 将 HTTP 代理转发到 Dante SOCKS5                     │
│     ├─ inotify 监听密码变更                                 │
│     └─ 端口 3128                                           │
│                                                            │
│  ⑤ 支撑容器                                               │
│     ├─ PostgreSQL 15                                       │
│     ├─ SWAG 反向代理                                       │
│     ├─ Watchtower 自动更新                                  │
│     └─ Autoheal 健康恢复                                    │
└────────────────────────────────────────────────────────────┘
```

### 2.2 各组件职责与代码量

| 组件 | 语言 | 核心文件 | 估算代码行 | 职责 |
|------|------|---------|-----------|------|
| HTTP API 服务 | JS | `api/worker.js`, `routes/worker/`, `routes/api/lease.js` | ~400 行 | 租约分配、配置返回、矿池注册 |
| WireGuard 管理 | JS | `wg-container.js`, `wireguard.js` | ~800 行 | 密钥轮换、配置读取/解析、容器交互、连接测试 |
| Dante/SOCKS5 管理 | JS | `dante-container.js` | ~350 行 | 凭证管理、租约分配、密码轮换 |
| 3proxy 配置 | Shell | `gen_config_and_start.sh` | ~100 行 | 动态生成 3proxy 配置、热重载 |
| 数据库层 | JS | `database/init.js`, `worker_wireguard.js`, `worker_socks5.js` | ~400 行 | 4 张表的 CRUD、事务锁、过期清理 |
| 缓存/工具 | JS | `caching.js`, `locks.js`, `validations.js` | ~200 行 | 内存缓存、互斥锁、模式验证 |
| 网络工具 | JS | `network.js`, `url.js`, `server.js` | ~200 行 | IP 提取、URL 构建、Express 配置 |
| 系统工具 | JS | `shell.js`, `process.js` | ~200 行 | Shell 执行、优雅关闭 |
| **总计** | | | **~2650 行 JS + ~100 行 Shell** | |

### 2.3 Worker 模式的关键行为特征

| 特征 | 描述 |
|------|------|
| **I/O 密集型** | 主要是 HTTP 请求、文件读写、数据库查询，几乎无 CPU 密集计算 |
| **并发模型** | 基于 async/await 的协作式并发，无多线程 |
| **系统调用依赖** | 重度依赖 `docker exec`、`wg`、`nc`、`ip` 等系统命令 |
| **状态管理** | PostgreSQL + 文件系统 + 内存缓存三层状态 |
| **外部通信** | 仅向矿池注册（出站 HTTP），接收矿池/Validator 请求（入站 HTTP） |

---

## 3. 逐组件 Rust 替代方案分析

### 3.1 组件 ① — Node.js HTTP API 服务 → Rust

**当前实现**: Express 5 + pg 驱动 + mentie 工具库

**Rust 替代**: 完全可行，且是收益最大的部分

| 功能点 | 当前实现 | Rust 方案 | 难度 |
|--------|---------|----------|------|
| HTTP 服务器 | Express 5 | `axum` 或 `actix-web` | ⭐ 低 |
| JSON 序列化 | 原生 JSON | `serde` + `serde_json` | ⭐ 低 |
| PostgreSQL | `pg` npm 包 | `sqlx` 或 `tokio-postgres` | ⭐ 低 |
| HTTP 客户端 | `fetch` API | `reqwest` | ⭐ 低 |
| 环境变量 | `process.env` | `std::env` 或 `dotenvy` | ⭐ 低 |
| 日志 | `mentie.log` | `tracing` + `tracing-subscriber` | ⭐ 低 |
| 优雅关闭 | 自定义信号处理 | `tokio::signal` | ⭐ 低 |
| 定时任务 | `setInterval` | `tokio::time::interval` | ⭐ 低 |
| UUID 生成 | `uuid` npm 包 | `uuid` crate | ⭐ 低 |

**评估**: ✅ 无障碍，Rust 异步生态完全覆盖

### 3.2 组件 ② — WireGuard 管理 → Rust

**当前实现**: 通过 `docker exec` 调用 WireGuard 容器内的 `wg` 命令行工具

**Rust 替代方案有两条路径**：

#### 路径 A：保留 WireGuard 容器，替换管理逻辑（保守）

| 功能点 | 当前实现 | Rust 方案 | 难度 |
|--------|---------|----------|------|
| Docker exec 调用 | `child_process.exec` | `tokio::process::Command` | ⭐ 低 |
| 配置文件读取/解析 | 正则 + 字符串处理 | `nom` 解析器或手写 parser | ⭐⭐ 中低 |
| 密钥生成 | `docker exec wg genkey` | `tokio::process::Command` | ⭐ 低 |
| 配置文件写入 | `fs.writeFile` | `tokio::fs` | ⭐ 低 |
| UDP 端口检测 | `nc -vzu` | `tokio::net::UdpSocket` | ⭐ 低 |
| .wg_ready 文件轮询 | `fs.stat` 循环 | `notify` crate (inotify) | ⭐ 低 |

**评估**: ✅ 低风险，行为与现有系统完全一致

#### 路径 B：内嵌 WireGuard，消除容器依赖（激进）

| 功能点 | Rust 方案 | 难度 |
|--------|----------|------|
| WireGuard 协议实现 | `boringtun`（Cloudflare 用户空间实现） | ⭐⭐⭐ 中高 |
| 密钥管理 | `x25519-dalek` | ⭐⭐ 中 |
| TUN 设备管理 | `tun` crate | ⭐⭐⭐ 中高 |
| 网络路由 | `rtnetlink` crate (netlink) | ⭐⭐⭐ 中高 |
| peer 管理 | 自定义实现 | ⭐⭐⭐ 中高 |

**评估**: ⚠️ 技术可行但风险较高，建议作为第二阶段目标

### 3.3 组件 ③ — Dante SOCKS5 代理 → Rust

**当前实现**: 独立 Dante 容器 + 密码文件交互

**Rust 替代方案有两条路径**：

#### 路径 A：保留 Dante 容器，替换管理逻辑（保守）

与当前行为一致，仅用 Rust 替换密码文件管理和租约逻辑。

**评估**: ✅ 低风险，约 1-2 天工作量

#### 路径 B：内嵌 SOCKS5 服务器（推荐）

| 功能点 | Rust 方案 | 难度 |
|--------|----------|------|
| SOCKS5 协议 | `fast-socks5` 或 `tokio-socks` | ⭐⭐ 中 |
| 认证管理 | 自定义用户名/密码验证 | ⭐ 低 |
| 连接池管理 | `tokio` 异步 TCP | ⭐⭐ 中 |
| 动态凭证轮换 | `tokio::sync::RwLock` 包裹凭证表 | ⭐ 低 |

**评估**: ✅ 推荐 — SOCKS5 协议简单（RFC 1928），Rust 实现成熟，可消除一个容器

```rust
// SOCKS5 认证核心逻辑示意
struct ProxyAuth {
    credentials: Arc<RwLock<HashMap<String, Credential>>>,
}

impl ProxyAuth {
    async fn authenticate(&self, user: &str, pass: &str) -> bool {
        let creds = self.credentials.read().await;
        creds.get(user)
            .map(|c| c.password == pass && c.available && c.expires_at > now_ms())
            .unwrap_or(false)
    }
    
    async fn rotate_password(&self, username: &str) -> Result<String> {
        let new_pass = generate_random_password();
        let mut creds = self.credentials.write().await;
        if let Some(cred) = creds.get_mut(username) {
            cred.password = new_pass.clone();
        }
        Ok(new_pass)
    }
}
```

### 3.4 组件 ④ — 3proxy HTTP 代理桥 → Rust

**当前实现**: 3proxy 将 HTTP CONNECT 请求转为 SOCKS5 上游连接

**Rust 替代**: ✅ 如果 SOCKS5 已内嵌，HTTP 代理桥可以一并实现

| 功能点 | Rust 方案 | 难度 |
|--------|----------|------|
| HTTP CONNECT 代理 | `hyper` + 自定义 `Service` | ⭐⭐ 中 |
| 上游 SOCKS5 转发 | 直接调用内嵌 SOCKS5 模块 | ⭐ 低（进程内调用） |
| Basic Auth | 自定义中间件 | ⭐ 低 |

**评估**: ✅ 如果选择路径 B（内嵌 SOCKS5），则 3proxy 可完全消除

```rust
// HTTP CONNECT 代理核心逻辑示意
async fn handle_connect(req: Request<Body>, auth: &ProxyAuth) -> Result<Response<Body>> {
    // 1. 验证 Proxy-Authorization 头
    let (user, pass) = extract_proxy_auth(&req)?;
    if !auth.authenticate(&user, &pass).await {
        return Ok(Response::builder().status(407).body(Body::empty())?);
    }
    
    // 2. 建立到目标的 TCP 连接（通过内嵌 SOCKS5 或直连）
    let target = req.uri().authority().unwrap();
    let upstream = TcpStream::connect(target.as_str()).await?;
    
    // 3. 升级连接为隧道
    tokio::spawn(async move {
        let (client, server) = tokio::io::copy_bidirectional(&mut upgraded, &mut upstream).await;
    });
    
    Ok(Response::new(Body::empty()))
}
```

### 3.5 数据库层 → Rust

**当前**: `pg` npm 包，4 张 Worker 相关表

| 表 | 操作 | Rust 方案 |
|----|------|----------|
| `worker_wireguard_configs` | INSERT/UPDATE/SELECT/DELETE，事务锁 | `sqlx` 编译期查询验证 |
| `worker_socks5_configs` | UPSERT/SELECT/UPDATE，唯一约束 | `sqlx` |
| `timestamps` | INSERT/UPDATE | `sqlx` |
| 初始化迁移 | CREATE TABLE/INDEX，向后兼容 | `sqlx::migrate!` |

**评估**: ✅ `sqlx` 提供编译期 SQL 验证，比 Node.js 字符串拼接更安全

```rust
// sqlx 编译期查询验证示例
let lease = sqlx::query_as!(
    WireguardLease,
    r#"
    INSERT INTO worker_wireguard_configs (id, expires_at, updated_at)
    SELECT s.id, $1, NOW()
    FROM generate_series(1, $2) AS s(id)
    WHERE s.id NOT IN (SELECT id FROM worker_wireguard_configs WHERE expires_at > $3)
    ORDER BY s.id
    LIMIT 1
    RETURNING id, expires_at
    "#,
    expires_at, peer_count, now_ms
).fetch_optional(&pool).await?;
```

---

## 4. 关键技术挑战

### 4.1 挑战 1：Linux 网络命名空间操作（难度 ⭐⭐⭐⭐）

**现状**: Worker 在 Miner/Validator 的评分流程中需要创建网络命名空间来测试 WireGuard 连接。但注意——**Worker 自身不执行这些测试**，这些代码运行在 Validator/Miner 模式。

**结论**: ⚠️ **Worker 模式不涉及此问题**。网络命名空间测试代码属于 Validator/Miner，不在 Worker 重构范围内。

### 4.2 挑战 2：Docker exec 容器交互（难度 ⭐⭐）

**现状**: Worker 通过 `docker exec wireguard <cmd>` 管理 WireGuard 密钥

```javascript
// 当前 JS 实现
await run(`docker exec wireguard wg genkey`)
await run(`docker exec wireguard wg pubkey`)  
await run(`docker exec wireguard wg set wg0 peer <key> remove`)
```

**Rust 方案**:

- **路径 A（保守）**: `tokio::process::Command` 1:1 替代
  ```rust
  let output = Command::new("docker")
      .args(["exec", "wireguard", "wg", "genkey"])
      .output().await?;
  ```
- **路径 B（激进）**: 使用 `bollard` crate 通过 Docker API 操作，避免 shell 注入风险
  ```rust
  let docker = Docker::connect_with_socket_defaults()?;
  let exec = docker.create_exec("wireguard", CreateExecOptions {
      cmd: Some(vec!["wg", "genkey"]),
      attach_stdout: Some(true),
      ..Default::default()
  }).await?;
  ```
- **路径 C（终极）**: 内嵌 WireGuard（消除容器），使用 `boringtun` 用户空间实现

### 4.3 挑战 3：文件系统状态同步（难度 ⭐⭐）

**现状**: Worker 与容器通过文件系统通信

```
/wg_configs/peer{N}/peer{N}.conf  ← WireGuard 容器生成，Node.js 读取
/passwords/{user}.password         ← Dante 容器管理，Node.js 读取
/passwords/{user}.password.used    ← Node.js 写入标记文件
/dante_regen_requests/{user}       ← Node.js 写入，Dante 容器监听
/.wg_ready                         ← WireGuard 容器写入就绪标志
```

**Rust 方案**: `tokio::fs` + `notify` crate（inotify 封装）

```rust
use notify::{Watcher, RecursiveMode, Event};

let mut watcher = notify::recommended_watcher(|res: Result<Event, _>| {
    if let Ok(event) = res {
        // 处理密码文件变更
    }
})?;
watcher.watch(Path::new("/passwords"), RecursiveMode::NonRecursive)?;
```

**评估**: ✅ 简单直接，Rust 的 `notify` crate 比 Node.js 的 `fs.watch` 更可靠

### 4.4 挑战 4：与 `mentie` 工具库的耦合（难度 ⭐⭐）

**现状**: Worker 代码大量依赖 `mentie` 库（Taofu 自研）

| `mentie` 功能 | 使用场景 | Rust 替代 |
|---------------|---------|----------|
| `cache()` | 内存缓存 + 磁盘持久化 | `dashmap` 或 `moka` + `serde_json` |
| `log` | 结构化日志 | `tracing` |
| `wait()` | 异步延迟 | `tokio::time::sleep` |
| `abort_controller()` | HTTP 超时 | `reqwest` 内建超时 |
| `make_retryable()` | 重试逻辑 | `backon` crate |
| `shuffle_array()` | Fisher-Yates 洗牌 | `rand::seq::SliceRandom` |
| `round_number_to_decimals()` | 数字格式化 | `format!("{:.2}", n)` |

**评估**: ✅ 全部有成熟 Rust 替代，且大部分更优

### 4.5 挑战 5：租约的原子性与竞态处理（难度 ⭐⭐⭐）

**现状**: Worker 使用了较精密的并发控制

```javascript
// 1. WireGuard 租约分配：数据库事务 + generate_series 找空位
INSERT INTO worker_wireguard_configs (id, expires_at, updated_at)
SELECT s.id, $1, NOW()
FROM generate_series(1, $2) AS s(id)
WHERE s.id NOT IN (SELECT id FROM worker_wireguard_configs WHERE expires_at > $3)
ORDER BY s.id LIMIT 1
RETURNING id, expires_at

// 2. 租约延期：乐观锁（WHERE expires_at = expected）
UPDATE worker_wireguard_configs
SET expires_at = $1, updated_at = NOW()
WHERE id = $2 AND expires_at = $3 AND expires_at > $4

// 3. 配置竞争检测：fire-and-forget 监听反馈 URL
monitor_lease_ownership({ peer_id, feedback_url, expires_at })
```

**Rust 方案**: `sqlx` 事务 + `tokio::sync::Mutex`

```rust
// 原子租约分配
async fn allocate_wireguard_lease(pool: &PgPool, lease_seconds: i64) -> Result<Lease> {
    let now = chrono::Utc::now().timestamp_millis();
    let expires_at = now + lease_seconds * 1000;
    let peer_count = get_wireguard_peer_count();
    
    // 单条 SQL 实现原子分配
    let lease = sqlx::query_as!(Lease, r#"
        INSERT INTO worker_wireguard_configs (id, expires_at, updated_at)
        SELECT s.id, $1, NOW()
        FROM generate_series(1, $2) AS s(id)
        WHERE s.id NOT IN (
            SELECT id FROM worker_wireguard_configs WHERE expires_at > $3
        )
        ORDER BY s.id LIMIT 1
        RETURNING id, expires_at as "expires_at!"
    "#, expires_at, peer_count as i32, now)
    .fetch_optional(pool).await?;
    
    lease.ok_or_else(|| anyhow!("All {} WireGuard slots exhausted", peer_count))
}
```

**评估**: ✅ Rust 的类型系统 + `sqlx` 编译期检查使竞态处理比 JS 更安全

---

## 5. Rust 生态库选型

### 5.1 推荐依赖清单

```toml
[dependencies]
# 异步运行时
tokio = { version = "1", features = ["full"] }

# Web 框架
axum = "0.8"
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "trace"] }

# 序列化
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# 数据库
sqlx = { version = "0.8", features = ["runtime-tokio", "tls-rustls", "postgres", "chrono"] }

# HTTP 客户端
reqwest = { version = "0.12", features = ["json"] }

# 日志
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# 工具
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
dotenvy = "0.15"
anyhow = "1"
thiserror = "2"

# 并发
dashmap = "6"          # 并发 HashMap（缓存）
moka = "0.12"          # 带 TTL 的缓存

# 文件系统
notify = "7"           # inotify 封装
tokio-stream = "0.1"   # 异步流

# 重试
backon = "1"           # 重试策略

# 随机
rand = "0.9"

# SOCKS5（如选择内嵌方案）
fast-socks5 = "0.9"

# Docker API（如选择 API 方式）
bollard = "0.18"

# WireGuard（如选择内嵌方案，第二阶段）
boringtun = "0.6"      # Cloudflare WireGuard 用户空间实现
x25519-dalek = "2"     # 密钥生成
```

### 5.2 各库成熟度评估

| 库 | GitHub Stars | 生产使用 | 维护状态 | 评估 |
|----|-------------|---------|---------|------|
| `axum` | 20k+ | Cloudflare, AWS | 活跃 | ✅ 首选 Web 框架 |
| `sqlx` | 14k+ | 广泛 | 活跃 | ✅ 编译期 SQL 验证 |
| `reqwest` | 10k+ | 事实标准 | 活跃 | ✅ 无争议 |
| `tracing` | 6k+ | Tokio 官方 | 活跃 | ✅ 无争议 |
| `boringtun` | 6k+ | Cloudflare 生产 | 维护中 | ⚠️ 功能够用但 API 偏底层 |
| `fast-socks5` | 200+ | 小规模 | 维护中 | ✅ 够用，协议简单可自实现 |
| `bollard` | 900+ | 中等 | 活跃 | ✅ Docker API 封装完整 |

---

## 6. 架构设计建议

### 6.1 推荐架构：统一 Rust 二进制

```
┌─ Rust Worker 二进制 ──────────────────────────────────────┐
│                                                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ axum HTTP 服务器                                      │  │
│  │  ├─ GET  /                    → 健康检查              │  │
│  │  ├─ GET  /api/lease/new       → 租约分配              │  │
│  │  ├─ GET  /api/stats           → 状态统计              │  │
│  │  ├─ POST /worker/register     → 注册接口              │  │
│  │  └─ GET  /ping                → IP 回显               │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  ┌────────────────┐  ┌────────────────┐  ┌──────────────┐ │
│  │ WireGuard 管理  │  │ SOCKS5 服务器   │  │ HTTP 代理    │ │
│  │ (容器交互 或    │  │ (内嵌)          │  │ (内嵌)       │ │
│  │  boringtun)    │  │ 端口 1080       │  │ 端口 3128    │ │
│  └────────┬───────┘  └────────┬───────┘  └──────┬───────┘ │
│           │                   │                  │          │
│  ┌────────┴───────────────────┴──────────────────┴───────┐ │
│  │ 共享状态层                                             │ │
│  │  ├─ sqlx::PgPool (PostgreSQL 连接池)                   │ │
│  │  ├─ LeaseManager (WG + SOCKS5 租约管理)                │ │
│  │  ├─ CredentialStore (Arc<RwLock<HashMap>>)             │ │
│  │  └─ AppConfig (环境变量 + 运行时配置)                   │ │
│  └────────────────────────────────────────────────────────┘ │
│                                                            │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ 后台任务 (tokio::spawn)                                │ │
│  │  ├─ 矿池注册守护 (每 60 分钟)                          │ │
│  │  ├─ 数据库清理守护 (每 300 秒)                          │ │
│  │  └─ 过期租约回收 (每 60 秒)                             │ │
│  └────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────┘

外部依赖（仅保留必要容器）:
  ├─ PostgreSQL 15
  ├─ WireGuard 容器 (第一阶段保留，第二阶段可选消除)
  └─ SWAG 反向代理
```

### 6.2 核心 struct 设计

```rust
/// 应用全局共享状态
#[derive(Clone)]
struct AppState {
    db: PgPool,
    config: Arc<AppConfig>,
    lease_manager: Arc<LeaseManager>,
    credentials: Arc<RwLock<CredentialStore>>,
}

/// Worker 配置
struct AppConfig {
    run_mode: RunMode,
    mining_pool_url: String,
    public_url: String,
    public_port: u16,
    wireguard_peer_count: u16,    // max 253
    wireguard_server_port: u16,   // default 51820
    socks5_port: u16,             // default 1080
    http_proxy_port: u16,         // default 3128
    priority_slots: usize,        // default 5
    payment_address_evm: Option<String>,
    payment_address_bittensor: Option<String>,
}

/// 租约管理器
struct LeaseManager {
    db: PgPool,
    wg_config_dir: PathBuf,
}

impl LeaseManager {
    async fn allocate_wireguard(&self, lease_seconds: i64, priority: bool) -> Result<WgLease>;
    async fn extend_wireguard(&self, peer_id: i32, expected_expires: i64, new_expires: i64) -> Result<WgLease>;
    async fn allocate_socks5(&self, lease_seconds: i64, priority: bool) -> Result<Socks5Lease>;
    async fn extend_socks5(&self, username: &str, expected_expires: i64, new_expires: i64) -> Result<Socks5Lease>;
    async fn cleanup_expired(&self) -> Result<CleanupStats>;
}
```

### 6.3 路由定义

```rust
fn worker_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(health_check))
        .route("/ping", get(ping))
        .route("/api/lease/new", get(lease_new))
        .route("/api/stats", get(stats))
        .route("/worker/register/force", post(force_register))
        .with_state(state)
}
```

---

## 7. 与现有系统的兼容性

### 7.1 协议兼容性（关键）

Rust Worker 必须与现有 JS Miner/Validator 完全兼容：

| 接口 | 方向 | 协议 | 兼容要求 |
|------|------|------|---------|
| `GET /` | 入站 | HTTP JSON | 返回相同的 `notice`, `version` 等字段 |
| `GET /api/lease/new` | 入站 | HTTP JSON/Text | 相同查询参数、相同响应格式、相同 HTTP 头 |
| `POST /miner/broadcast/worker` | 出站 | HTTP JSON | 相同的注册 payload 结构 |
| `GET /protocol/stats` | 入站 | HTTP JSON | Worker 模式无此端点（仅 Validator/Miner） |
| `X-Lease-Ref` 响应头 | 入站 | HTTP | 必须保持一致 |
| `X-Lease-Expires` 响应头 | 入站 | HTTP | 必须保持一致 |
| `X-Lease-Extension-Token` 响应头 | 入站 | HTTP | 必须保持一致 |

### 7.2 数据库兼容性

Rust Worker 与 JS Miner/Validator 共享同一 PostgreSQL 实例：

- **Worker 独有表**: `worker_wireguard_configs`, `worker_socks5_configs` — 可自由演进
- **共享表**: `timestamps` — 必须保持 schema 兼容
- **初始化逻辑**: 必须保留向后兼容迁移逻辑

### 7.3 文件系统接口兼容性

如果保留 WireGuard/Dante 容器：

| 路径 | 读/写 | 兼容要求 |
|------|-------|---------|
| `/wg_configs/peer{N}/peer{N}.conf` | 读 | WireGuard 容器写入格式不变 |
| `/wg_configs/.wg_ready` | 读 | 就绪标志文件 |
| `/passwords/{user}.password` | 读 | Dante 凭证文件格式 |
| `/passwords/{user}.password.used` | 写 | 租约标记文件 |
| `/dante_regen_requests/{user}` | 写 | 密码轮换触发文件 |

### 7.4 Docker 兼容性

Rust 二进制可以直接替换 `tpn-federated` 容器中的 Node.js 进程：

```dockerfile
# 替换前
FROM node:20-alpine
COPY . /app
CMD ["node", "app.js"]

# 替换后
FROM debian:bookworm-slim
COPY tpn-worker /usr/local/bin/
CMD ["tpn-worker"]
```

镜像大小变化预估：**~300MB → ~20MB**（静态链接的 Rust 二进制）

---

## 8. 工作量估算

### 8.1 分阶段工作量

#### 阶段 1：HTTP API + 数据库 + 矿池注册（替换 Node.js 核心）

| 任务 | 预估工时 | 说明 |
|------|---------|------|
| 项目脚手架（Cargo.toml, 模块结构） | 2h | |
| AppConfig 和环境变量加载 | 4h | |
| 数据库初始化 + 迁移逻辑 | 8h | 4 张 Worker 表 + 索引 + 向后兼容 |
| 健康检查端点 `GET /` | 2h | |
| 租约分配 `GET /api/lease/new` | 16h | WireGuard + SOCKS5 双路径 |
| 租约延期逻辑 | 8h | 原子性保证、竞态处理 |
| WireGuard 配置读取/解析 | 8h | 保留容器交互 |
| SOCKS5 凭证管理 | 8h | 文件读取 + 数据库同步 |
| 矿池注册守护进程 | 4h | |
| 数据库清理守护进程 | 2h | |
| 优雅关闭处理 | 2h | |
| 集成测试 | 16h | 与现有 Miner/Validator 联调 |
| **小计** | **~80h (2 周)** | |

#### 阶段 2：内嵌 SOCKS5 + HTTP 代理（消除 Dante 和 3proxy 容器）

| 任务 | 预估工时 | 说明 |
|------|---------|------|
| SOCKS5 服务器实现 | 16h | 基于 `fast-socks5` 或自实现 RFC 1928 |
| 用户名/密码认证模块 | 8h | 动态凭证管理 |
| HTTP CONNECT 代理实现 | 12h | 基于 `hyper` |
| 凭证热轮换逻辑 | 4h | 替代文件系统信号机制 |
| 压力测试 + 兼容测试 | 12h | |
| **小计** | **~52h (1.5 周)** | |

#### 阶段 3（可选）：内嵌 WireGuard（消除 WireGuard 容器）

| 任务 | 预估工时 | 说明 |
|------|---------|------|
| `boringtun` 集成 | 24h | 用户空间 WireGuard |
| TUN 设备管理 | 16h | 需要 NET_ADMIN 能力 |
| 密钥管理（生成/轮换） | 8h | x25519 |
| peer 动态管理 | 16h | 添加/移除/重配 |
| 路由和 DNS 配置 | 8h | netlink 操作 |
| 回归测试 | 16h | 大量边界情况 |
| **小计** | **~88h (2+ 周)** | |

### 8.2 总工作量

| 方案 | 工时 | 人月 | 容器数变化 |
|------|------|------|-----------|
| 仅阶段 1 | ~80h | ~0.5 | 4 → 4（替换 Node.js，保留其他） |
| 阶段 1+2 | ~132h | ~0.8 | 4 → 2（消除 Dante + 3proxy） |
| 阶段 1+2+3 | ~220h | ~1.4 | 4 → 1（全部内嵌） |

> 以上估算基于有 Rust 异步编程经验的工程师。如果团队 Rust 经验不足，应额外增加 50% 的学习曲线时间。

---

## 9. 收益分析

### 9.1 资源消耗对比

| 指标 | 当前 (Node.js + 3 容器) | Rust (阶段 1+2) | 提升倍数 |
|------|------------------------|-----------------|---------|
| **内存占用** | ~400-600MB (4 容器合计) | ~30-50MB (1 进程) | **10-12x** |
| **启动时间** | ~30-60s (等待所有容器) | ~1-2s | **20-30x** |
| **Docker 镜像大小** | ~800MB (4 镜像合计) | ~20MB (静态二进制) | **40x** |
| **CPU 空闲占用** | ~2-5% (V8 GC + 进程管理) | ~0.1% | **20-50x** |
| **文件描述符** | ~200+ (多进程) | ~50 | **4x** |

### 9.2 安全性提升

| 方面 | 当前 | Rust |
|------|------|------|
| 内存安全 | V8 GC 保护，但原生依赖可能存在问题 | 编译期保证，无 GC |
| SQL 注入 | 字符串拼接（依赖开发者自律） | `sqlx` 编译期参数化查询验证 |
| 命令注入 | `child_process.exec` 有注入风险 | `Command::new` 参数化，无 shell |
| 类型安全 | 运行时类型检查 | 编译期类型检查 |
| 依赖安全 | npm 供应链风险较高 | Cargo 生态相对安全，`cargo audit` |

### 9.3 运维提升

| 方面 | 当前 | Rust |
|------|------|------|
| 部署 | 4 个 Docker 镜像 + 编排 | 1 个静态二进制（或 1 个极小镜像） |
| 更新 | Watchtower 监控多个镜像 | 单个二进制替换 |
| 调试 | 4 套日志流 | 1 套结构化日志 |
| 监控 | 多进程健康检查 | 单进程，内建指标 |
| 崩溃恢复 | Autoheal 重启容器 | 单进程 panic 恢复或 systemd 重启 |

### 9.4 开发体验提升

| 方面 | 当前 | Rust |
|------|------|------|
| 重构信心 | 低（运行时类型错误） | 高（编译器是第一道防线） |
| 并发安全 | 依赖开发者经验 | 编译器强制 `Send + Sync` |
| 文档 | JSDoc（可选） | `rustdoc`（集成在工具链中） |
| 依赖管理 | `package-lock.json` 冲突频繁 | `Cargo.lock` 更稳定 |

---

## 10. 风险评估

### 10.1 高风险

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| **WireGuard 容器交互的时序问题** | 密钥轮换期间可能丢失连接 | 保持与现有 JS 相同的容器操作序列；添加回滚逻辑 |
| **租约竞态条件** | 分配相同 peer 给两个客户端 | 使用 `sqlx` 事务 + `SELECT FOR UPDATE`；全面的并发测试 |
| **与 Miner/Validator 的协议偏移** | Worker 注册失败、配置不被接受 | 编写 JSON schema 契约测试；在 CI 中联调 |

### 10.2 中风险

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| **Rust 团队经验不足** | 开发周期延长 50-100% | 先从阶段 1 开始，积累经验后再推进 |
| **`boringtun` 兼容性问题** | 无法替代内核 WireGuard | 阶段 3 标记为可选；保留容器回退方案 |
| **SOCKS5 边缘协议行为** | 某些客户端不兼容 | 使用成熟库 `fast-socks5`；抓包对比测试 |

### 10.3 低风险

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| Rust 编译时间较长 | 开发迭代变慢 | 使用 `cargo-watch`；合理拆分 crate |
| 跨平台问题 | 目标仅 Linux | Rust 交叉编译成熟；CI 中用 `cross` |
| PostgreSQL 驱动兼容性 | 极低概率 | `sqlx` 是最成熟的 Rust PG 驱动 |

---

## 11. 推荐实施路径

### 11.1 路线图

```
                        月 1                月 2                月 3
                   ┌─────────────────┬─────────────────┬─────────────────┐
 阶段 1 (核心替换)  │████████████████ │                 │                 │
                   │ HTTP API + DB   │                 │                 │
                   │ + 矿池注册       │                 │                 │
                   │ + WG 容器管理    │                 │                 │
                   ├─────────────────┤                 │                 │
 联调测试           │        █████████│                 │                 │
                   ├─────────────────┼─────────────────┤                 │
 阶段 2 (内嵌代理)  │                 │████████████████ │                 │
                   │                 │ 内嵌 SOCKS5      │                 │
                   │                 │ + HTTP CONNECT   │                 │
                   ├─────────────────┼─────────────────┼─────────────────┤
 阶段 3 (内嵌 WG)  │                 │                 │█████████████████│
  (可选)           │                 │                 │ boringtun 集成   │
                   └─────────────────┴─────────────────┴─────────────────┘
```

### 11.2 阶段 1 详细步骤

```
Week 1:
  ├─ Day 1-2: 项目初始化，AppState/AppConfig，环境变量加载
  ├─ Day 3:   数据库连接池，表初始化，迁移逻辑
  ├─ Day 4-5: 健康检查端点，WireGuard 配置文件解析器

Week 2:
  ├─ Day 1-2: WireGuard 租约分配（DB 事务 + generate_series）
  ├─ Day 3:   WireGuard 租约延期（乐观锁）
  ├─ Day 4:   SOCKS5 凭证加载 + 租约分配
  ├─ Day 5:   SOCKS5 租约延期 + 密码轮换

Week 3:
  ├─ Day 1:   /api/lease/new 完整端点（两种类型 + 两种格式）
  ├─ Day 2:   矿池注册守护进程 + 定时任务框架
  ├─ Day 3-5: 集成测试（与现有 Miner/Validator JS 联调）

Week 4:
  ├─ Day 1-2: Bug 修复 + 边界情况处理
  ├─ Day 3:   Docker 镜像构建 + CI 集成
  ├─ Day 4-5: Staging 环境验证
```

### 11.3 验收标准

每个阶段完成时必须满足：

1. **功能等价**: 所有现有测试用例通过（移植 `federated-container/test/4.x-worker.*.test.js`）
2. **协议兼容**: JS Miner 可以正常与 Rust Worker 交互（注册、租约请求）
3. **性能不退化**: 租约分配延迟 ≤ 当前 Node.js 版本
4. **资源改善**: 内存占用 ≤ 100MB
5. **零停机切换**: 可通过修改 `docker-compose.yml` 的 `image` 标签无缝切换

---

## 12. 结论

### 12.1 核心判断

| 问题 | 结论 |
|------|------|
| **技术上是否可行？** | ✅ 完全可行。Worker 的所有功能在 Rust 生态中都有成熟的替代方案 |
| **是否值得做？** | ✅ 值得。Worker 是部署量最大的节点类型（多对一），资源节约效果被放大 |
| **最大的技术障碍是什么？** | WireGuard 容器交互的密钥轮换逻辑（但可分阶段解决） |
| **能否渐进式迁移？** | ✅ 可以。Rust Worker 只要暴露相同的 HTTP API，对 Miner/Validator 完全透明 |
| **推荐从哪里开始？** | 阶段 1（HTTP API + DB + WG 容器管理），已可独立交付价值 |

### 12.2 决策矩阵

```
                    高收益
                      │
      阶段 2          │         阶段 1
   (内嵌 SOCKS5)      │      (核心替换)
   ✅ 推荐            │      ✅ 强烈推荐
                      │
  ────────────────────┼──────────────────── 低风险
                      │
      阶段 3          │
   (内嵌 WireGuard)   │
   ⚠️ 可选            │
                      │
                    低收益
```

### 12.3 最终建议

**推荐执行阶段 1 + 阶段 2**，总计约 132 工时（~1 人月）：

- 将 Worker 从 **4 个容器** 精简为 **1 个 Rust 二进制 + PostgreSQL + WireGuard 容器 + SWAG**
- 内存占用从 **~500MB** 降至 **~40MB**
- Docker 镜像从 **~800MB** 降至 **~20MB + WG 镜像**
- 消除 Dante 和 3proxy 两个外部依赖
- 保持与现有 Miner/Validator 的完全兼容

阶段 3（内嵌 WireGuard）标记为**可选优化**，在阶段 1+2 稳定运行后根据实际需求决定是否推进。
