# Project Architecture Plan & Implementation Roadmap (AGENTS_PLAN.md)

本文件用于规划和追踪 Proxy Gateway 项目的架构设计、阶段性演进目标、技术决策与实施进度。

---

## 一、阶段演进路线概览

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Proxy Gateway 演进路线图                         │
└────────────────────────────────────────────────────────────────────────┘
                                    │
    ┌───────────────────────────────┼──────────────────────────────┐
    ▼                               ▼                              ▼
【阶段 1：调度与数据面强化】     【阶段 2：控制面与订阅解析】    【阶段 3：Vue 3 + Bun 控制台】
 • 纯内存 LeaseTable            • Axum REST 控制面             • Bun + Vite + Vue 3 搭建
 • EgressIP O(1) 备用漂移       • 节点列表与单节点测活接口     • 实时指标看板与交互
 • ArcSwap 前缀树 Trie 规则     • 规则沙箱测试接口 (/resolve)   • 规则调试器与抽屉管理
 • 协议嗅探与延迟 Commit        • Base64 订阅拉取与调度        • rust-embed 单二进制融合
 ( [x] 已完成并提交 )           ( [x] 已完成并提交 )           ( [x] 已完成并提交 )
```

---

## 二、详细实施计划与当前进展

### 阶段 1：调度与数据面强化 (已完成并已独立 Commit)
- [x] **纯内存 Sticky Session 租约与平滑漂移 (`src/pool/lease.rs`)**：
  - 基于 `DashMap<AccountId, Lease>` 实现纯内存租约管理，零磁盘写盘，彻底切断 Linux 内核 D 状态与高 CPU 抖动源头；
  - 维护 `EgressIpIndex` 反向索引 (`DashMap<IpAddr, SmallVec<[usize; 4]>>`)，当原节点下线时以 $O(1)$ 时间复杂度快速找到同出口 IP 的备用节点，避免全表扫描；
  - 惰性过期与后台自动定期扫描淘汰。
- [x] **零侵入 URL 前缀 Trie 树 (`src/reverse/rules.rs`)**：
  - 实现最长前缀匹配树，Host 小写规范化、剥离端口与 IPv6 压缩、Path 严格区分大小写且剥离 query；
  - 基于 `ArcSwap<AccountMatcher>` 实现无锁并发读与 Copy-On-Write 原子热更新；
  - 支持大小写不敏感 Headers 优先级链式探测与账号提取。
- [x] **全链路测试覆盖 (`tests/gateway_v1.rs`)**：
  - 粘性会话与同 IP 备用节点漂移测试；
  - 前缀树规则最长匹配与 Headers 账号提取测试。

---

### 阶段 2：控制面 REST API 与订阅解析器 (已完成并已独立 Commit)
- [x] **Axum RESTful 控制面 (`src/api/mod.rs`)**：
  - `GET /api/v1/system/info`：网关健康状态、活跃节点数、活跃租约数、免认证模式探测；
  - `GET /api/v1/nodes`：服务端内存分页，按状态（healthy / cooldown / all）及关键词过滤；
  - `POST /api/v1/nodes/:id/actions/probe`：单节点手动实时测活（真实 HTTP 探测并回显延迟与状态）；
  - `GET /api/v1/rules` & `PUT /api/v1/rules/:prefix`：规则查看与原子替换；
  - `POST /api/v1/rules:resolve`：规则调试器沙箱（接收 URL + 模拟 Headers，实时回显命中的规则前缀与提取出的 Account）；
  - `POST /api/v1/subscriptions/sync`：订阅内容解析并热重载入池。
- [x] **通用订阅解析器 (`src/subscription/mod.rs`)**：
  - 支持 Base64 编码的订阅正文解析；
  - 节点配置解析（HTTP / SOCKS5 / SOCKS5h）与预解析拨号 IP。

---

### 阶段 3：Vue 3 + Bun 现代化前端 Web UI 与单二进制打包 (已完成)
- [x] **前端工程搭建 (Bun + Vite + Vue 3 + TypeScript + Tailwind v4)**：
  - 现代化仪表盘看板（节点健康度分布、监控总览、纯内存零 D 状态架构说明）；
  - 节点列表管理（状态筛选、手动测活按钮与 Toast 回显）；
  - 规则调试器（输入测试 URL 与 Header，实时调试提取结果）；
  - 订阅管理（一键导入 Base64 格式或远程链接并热重载入池）。
- [x] **单二进制融合 (`rust-embed`)**：
  - 将编译产物 `webui/dist` 嵌入 Rust 二进制文件（产物 gzip 仅约 38 KB）；
  - 实现 SPA History Fallback（非资源路由全部回退到 `index.html`）；
  - 单个可执行文件开箱即用，兼具高性能代理网关与管理面板。
- [x] **Obscura 真实浏览器端到端自动化验证**：
  - 通过 Obscura Headless 浏览器导航 `/ui/`，验证 Dashboard 卡片、Node Pool 节点表、Rules 交互沙箱；
  - 自动捕获全高全宽控制台截图并输出至 `docs/images/dashboard.png`，在 `README.md` 中展示。

---

## 三、底层网络协议与网关核心功能矩阵 (Core Matrix)

### 1. 协议接入与语义修正 (Inbound & Protocol Correctness)
- [x] **消除早回复 (Delayed Commit 时序铁律)**：SOCKS5 与 HTTP CONNECT 握手必须且只能在 upstream 连接成功后才向下游 commit 成功回复；拨号失败时向下游返回 502/SOCKS REP 错误码，保证客户端重试机制有效。
- [x] **单字节嗅探与 TCP 分包防御 (Sniff & Anti-fragmentation)**：
  - 首包仅 peek 1 字节做 `0x05` (SOCKS5) 与非 `0x05` (HTTP) 二分流；
  - HTTP 流量由 HTTP 状态机完整缓冲请求头后再区分 CONNECT（正向代理）与 GET/POST（反向代理），彻底规避 TCP 拆包误判。
- [x] **HTTP CONNECT 规范解析**：引入 `httparse`，支持大小写不敏感 headers、IPv6 authority `[::1]:port`，RFC Basic 认证。
- [x] **SOCKS5 Method 协商与 RFC 1929 认证**：规范客户端方法选择与鉴权失败时 `[0x01, 0x01]` 拒绝。
- [x] **内存安全与类型安全**：彻底杜绝 `from_utf8_unchecked` 等 UB 风险，采用强类型 `TargetAddr` 与 `TargetHost`。

### 2. 代理池韧性与调度 (Proxy Pool Subsystem)
- [x] **无锁并发快照 (`ArcSwap<PoolSnapshot>`)**：支持热加载代理节点列表，零读锁开销。
- [x] **DNS 预解析与语义分离**：
  - 上游节点地址加载时预解析为 `Arc<[SocketAddr]>`，避免拨号时的 DNS 阻塞；
  - 目标地址（`TargetAddr`）若为 SOCKS5h 则保持域名由远端解析。
- [x] **智能选路策略 (Selection Strategies)**：
  - RoundRobin (轮询)
  - Random (随机)
  - Power of Two Choices (P2C，基于连续失败和活跃连接数的最优选路)
- [x] **被动熔断与退避冷却 (Passive Circuit Breaker & Cooldown)**：
  - 连续失败超过阈值自动移入冷却；
  - 指数退避冷却恢复与半开探针（HalfOpen）。
- [x] **透明故障重试 (Transparent Retry)**：在向下游 commit 之前，遇 `is_retryable` 错误自动轮换候选节点重试。

### 3. 反向代理子系统 (Reverse Proxy Subsystem)
- [x] **单端口多协议共享分流**：HTTP 非 CONNECT 请求无缝转入反向代理管道。
- [x] **路径路由与 Host 头分流**：
  - `/target/{host:port}/{path}` 格式动态路由；
  - `Host` 头部解析与代理池动态转发；
  - 请求头重写与全双工数据流透传。

### 4. 自动化构建与发布 (CI/CD)
- [x] **每日自动定时构建与 Release 工作流 (`.github/workflows/daily_release.yml`)**：
  - GitHub Actions 配置：每天 UTC+8 凌晨 2:17 (UTC 18:17) 定时触发；
  - 支持手动触发 (`workflow_dispatch`)；
  - 自动编译 release 二进制并发布 GitHub Release。

---

## 四、后续演进计划 (Future Work)
- [ ] **P2C 选路结合 TD-EWMA 延迟加权**：在两个候选节点中结合网络延迟与连续失败惩罚算分。
- [ ] **Prometheus 内存指标收集 (Metrics Manager)**：纯内存滑动窗口直方图与 QPS 统计。
- [ ] **Cuckoo Filter / RoaringBitmap 目标站黑名单快速过滤**。
