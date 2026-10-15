# Project Roadmap & Progress Tracking (AGENTS_TODO.md)

本文件用于跨会话持续追踪核心功能、系统重构、测试与部署的实现进度。

## 一、协议接入与语义修正 (Inbound & Protocol Correctness)
- [x] **消除早回复 (Delayed Commit)**：SOCKS5 与 HTTP CONNECT 握手必须在 upstream 连接成功后才向下游 commit 成功回复；失败时向下游返回 502/SOCKS REP 错误码，保证重试有效。
- [x] **单字节嗅探与 TCP 分包防御 (Sniff & Anti-fragmentation)**：
  - 仅 peek 1 字节区分 `0x05` (SOCKS5) 与其它 (HTTP)；
  - HTTP 流量由 HTTP 状态机完整缓冲请求头后再区分 CONNECT（正向代理）与 GET/POST（反向代理），彻底规避 TCP 拆包误判。
- [x] **HTTP CONNECT 规范解析**：引入 `httparse`，支持大小写不敏感 headers、IPv6 authority `[::1]:port`，RFC Basic 认证。
- [x] **SOCKS5 Method 协商与 RFC 1929 认证**：规范客户端方法选择与鉴权失败时 `[0x01, 0x01]` 拒绝。
- [x] **全面移除 `from_utf8_unchecked`**：彻底杜绝 UB 风险，使用强类型 `TargetAddr` 与 `TargetHost`。

## 二、代理池核心子系统 (Proxy Pool Subsystem)
- [x] **无锁并发快照 (ArcSwap<PoolSnapshot>)**：支持热加载代理节点列表，零读锁开销。
- [x] **DNS 预解析与语义分离**：
  - 上游节点地址加载时预解析为 `Arc<[SocketAddr]>`，避免拨号时的 DNS 阻塞；
  - 目标地址（TargetAddr）若为 SOCKS5h 则保持域名由远端解析。
- [x] **智能选路策略 (Selection Strategies)**：
  - RoundRobin (轮询)
  - Random (随机)
  - Power of Two Choices (P2C，基于连续失败和活跃连接数的最优选路)
- [x] **被动熔断与退避冷却 (Passive Circuit Breaker & Cooldown)**：
  - 连续失败超过阈值自动移入冷却；
  - 指数退避冷却恢复与半开探针（HalfOpen）。
- [x] **透明故障重试 (Transparent Retry)**：在向下游 commit 之前，遇 `is_retryable` 错误自动轮换候选节点重试。

## 三、反向代理子系统 (Reverse Proxy Subsystem)
- [x] **单端口多协议共享分流**：HTTP 非 CONNECT 请求无缝转入反向代理管道。
- [x] **路径路由与 Host 头分流**：
  - `/target/{host:port}/{path}` 格式动态路由；
  - `Host` 头部解析与代理池动态转发；
  - 请求头重写与全双工数据流透传。

## 四、自动化构建与发布 (CI/CD)
- [x] **每日自动定时构建与 Release 工作流**：
  - GitHub Actions 配置：每天 UTC+8 凌晨 2:17 (UTC 18:17) 定时触发；
  - 支持手动触发 (`workflow_dispatch`)；
  - 自动编译 release 二进制并发布 GitHub Release。

## 五、后续演进计划 (Future Work & Resin 对比吸纳)
- [ ] **内存级会话粘性租约 (Memory-Only Sticky Session)**：
  - 参考 Resin 的 `Platform.Account -> Egress IP (Lease)` 机制；
  - 纯内存 `DashMap` 实现，严格禁止磁盘写入，彻底杜绝 D 状态风险。
- [ ] **P2C 选路结合 TD-EWMA 延迟加权**：在两个候选节点中结合网络延迟与连续失败惩罚算分。
- [ ] **外部订阅源拉取 (Subscription Puller)**：支持从 HTTP(S) 订阅链接拉取 Base64 节点列表并热重载入 `ArcSwap`。
- [ ] **Prometheus 内存指标收集 (Metrics Manager)**：纯内存滑动窗口直方图与 QPS 统计。
- [ ] **Cuckoo Filter / RoaringBitmap 目标站黑名单快速过滤**。
