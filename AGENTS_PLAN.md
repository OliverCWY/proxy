# Project Architecture Plan & Implementation Roadmap (AGENTS_PLAN.md)

本文件用于规划和追踪 Proxy Gateway 项目的架构设计、阶段性演进目标、技术决策与实施进度。

---

## 一、阶段演进路线概览

```text
┌────────────────────────────────────────────────────────────────────────────────────────┐
│                        Proxy Gateway 终极演进路线图                                     │
└────────────────────────────────────────────────────────────────────────────────────────┘
                                            │
    ┌───────────────────────────────┬───────┴───────────────────────┬───────────────────────────────┐
    ▼                               ▼                               ▼                               ▼
【阶段 1~3：核心基础设施】       【阶段 4：单端口与 Resin 规范】  【阶段 5：三层立体熔断拉黑】    【阶段 6~8：平台视图与前端进化】
 • 纯内存 LeaseTable/Trie        • 单端口多路复用 (2260)          • 节点连续失败熔断/退避/半开    • 多订阅不可变 Hash 去重
 • Axum 控制面 REST API          • 双 Token 鉴权隔离 (Admin/Proxy)• 目标站 SSRF 与域名黑名单快速阻断• Platform 虚拟可路由视图
 • Vue 3 嵌入式单二进制 Web UI   • Resin 规范反代 URL 路径解析    • 脏节点永久驱逐/拉黑与洗白     • Web UI 接入面板/移动端/i18n
 ( [x] 已完成并提交 )            ( [x] 已完成并提交 )             ( [ ] 下一步实施 )              ( [ ] 规划中 )
```

---

## 二、已完成阶段回顾 (Completed Milestones)

### 阶段 1：调度与数据面强化 (Commit: `f7c7c94c`)
- [x] **纯内存 Sticky Session 租约与平滑漂移 (`src/pool/lease.rs`)**：
  - 基于 `DashMap<AccountId, Lease>` 实现纯内存租约管理，零磁盘写盘，切断 Linux 内核 D 状态与高 CPU 抖动源头；
  - 维护 `EgressIpIndex` 反向索引 (`DashMap<IpAddr, SmallVec<[usize; 4]>>`)，在 $O(1)$ 时间复杂度快速找到同出口 IP 的备用节点，避免全表扫描；
  - 惰性过期与后台自动定期扫描淘汰。
- [x] **零侵入 URL 前缀 Trie 树 (`src/reverse/rules.rs`)**：
  - 实现最长前缀匹配树，Host 小写规范化、剥离端口与 IPv6 压缩、Path 严格区分大小写且剥离 query；
  - 基于 `ArcSwap<AccountMatcher>` 实现无锁并发读与 Copy-On-Write 原子热更新；
  - 支持大小写不敏感 Headers 优先级链式探测与账号提取。

### 阶段 2：控制面 REST API 与订阅解析器 (Commit: `745ccbf1`)
- [x] **Axum RESTful 控制面 (`src/api/mod.rs`)**：
  - `GET /api/v1/system/info`、`GET /api/v1/nodes`（内存分页与状态过滤）、`POST /api/v1/nodes/:id/actions/probe`（单节点手动测活）；
  - `GET /api/v1/rules`、`PUT /api/v1/rules/:prefix`、`POST /api/v1/rules:resolve`（规则沙箱调试）；
  - `POST /api/v1/subscriptions/sync`（订阅内容导入与热重载）。
- [x] **通用订阅解析器 (`src/subscription/mod.rs`)**：
  - 支持 Base64 编码与多行文本解析，支持 HTTP / SOCKS5 / SOCKS5h 节点及拨号 IP 预解析。

### 阶段 3：Vue 3 现代化单二进制控制台与调度增强 (Commit: `81d8ddd6` & `3ba7d5e4`)
- [x] **Bun + Vite + Vue 3 + Tailwind CSS v4 嵌入式前端**：
  - 超轻量构建（完整 bundle gzip 仅 ~38KB）；
  - 单二进制内嵌（`rust-embed`）与 SPA History Fallback；
  - Obscura 自动化测试验收并生成 `docs/images/` 截图。
- [x] **P2C 选路结合 TD-EWMA 延迟加权 (`src/pool/selector.rs`, `src/pool/node.rs`)**：
  - 实时拨号 RTT 测算与时间衰减半衰期 $\tau=10s$ 平滑指数加权；
  - 综合打分公式 $(\text{EWMA} + 1) \cdot (\text{ActiveConns} + 1) \cdot 2^{\text{Failures}}$。
- [x] **Prometheus 内存指标收集 (`src/api/mod.rs` `/metrics`)**：
  - 纯内存流式导出 `proxy_nodes_total`、`proxy_nodes_healthy`、`proxy_sticky_leases_active`、`proxy_node_latency_ewma_ms` 等。

---

## 三、当前与未来演进计划 (Detailed Roadmap)

### 阶段 4：单端口四合一聚合与 Resin 规范反代管道 (Commit: `08b84c03`)
- [x] **单端口多协议自动分流器 (Single-Port Multiplexer)**：
  - 全局仅暴露单个主端口（默认 2260，兼容 1099 与自定义环境变量）；
  - 1 字节 Peek 区分 `0x05`（SOCKS5）与 HTTP；
  - HTTP 首部状态机分流：
    - `CONNECT` 请求 $\to$ 正向 HTTP 隧道代理；
    - 路径以 `/ui`、`/api`、`/metrics`、`/healthz` 开头 $\to$ 控制面与内嵌 Web UI；
    - 其余路径形态 $\to$ 走 Resin 规范反向代理管道。
- [x] **双 Token 鉴权体系隔离**：
  - `ADMIN_TOKEN`：保护控制面 API 与 Web UI 登录（Bearer 认证），未配置时自动开启免认证模式；
  - `PROXY_TOKEN`：代理转发鉴权密钥（反代放 URL 第一段，正代放 Basic Auth 密码位）。
- [x] **Resin 规范反代 URL 路径解析器 (V1 协议标准)**：
  - 接入规范格式：`/{PROXY_TOKEN}/{Platform.Account}/{protocol}/{target_host}/{target_path...}?{query}`；
  - 第 1 段 Token 快速校验，不匹配直接 `403 AUTH_FAILED`；
  - 第 2 段按 `.` 拆分平台与业务账号（支持 `:` 兼容与三级空账号回落）；
  - 第 3 段校验 `http`/`https`，第 4 段 Host 合法性与 IPv6 方括号规范化，第 5 段保留原始转义与 Query 透传。

### 阶段 5：三层立体熔断与拉黑防御体系
- [ ] **第一层：节点连续失败熔断与指数退避冷却 (Node Passive Circuit Breaker)**：
  - 网络错误（超时、RST、EOF）连续累加，达阈值（默认 3 次）自动移入 `Cooldown`；
  - 指数退避冷却（$5\text{s} \to 10\text{s} \to \dots$ 上限 5 分钟），期满进入 `HalfOpen` 放行探测流量；
  - 支持平台级 `passive_circuit_breaker_disabled` 免熔断开关。
- [ ] **第二层：脏节点永久拉黑与垃圾回收驱逐 (Permanent Eviction & Blocklist)**：
  - 长期处于熔断超时（如连续 72 小时）的临时节点自动从内存全局池物理驱逐（GC）；
  - 管理员可在 Web UI / API 对异常节点一键“拉黑 (Block)”与“洗白 (Unblock)”。
- [ ] **第三层：目标域名/安全黑名单快速拦截 (Target Blacklist & SSRF Guard)**：
  - 内网私有网段拦截（SSRF 防御：`127.0.0.1`, `10.0.0.0/8`, `192.168.0.0/16`, `169.254.169.254`）；
  - 目标站封禁域名快速阻断（基于 Domain Suffix Trie / CIDR，微秒级直接返回 403，零连接消耗）。

### 阶段 6：多订阅源不可变 Hash 去重与 Platform 虚拟路由视图
- [ ] **不可变 NodeHash 去重引擎**：
  - 剔除 `tag` 展示名，按不可变核心网络参数提取规范 JSON 并计算 XXH3-128；
  - 跨订阅源同 Hash 节点在全局池中唯一存储，引用归零时物理清理。
- [ ] **Platform 虚拟路由视图 (`ArcSwap<PlatformView>`)**：
  - 平台支持按 Tag 正则、Region 地区代码、Egress IP、健康状态等多级过滤构建只读视图；
  - 节点状态变化触发无锁 COW 热替换。

### 阶段 7：系统级硬盘压力监测与冷持久化（彻底切断 D-State）
- [ ] **系统级硬盘压力与 D-State 探针**：
  - 采集 Linux `/proc/diskstats` 与系统 `iowait` 比例；
  - 监控并暴露操作系统处于 `D-state`（Uninterruptible Sleep）的线程数；
  - 在 Prometheus 与 Web 控制台显式证明数据面“零磁盘写入”与“零 D-State”。
- [ ] **冷数据嵌入式持久化 (`redb` / `rusqlite` WAL)**：
  - 仅用于持久化冷配置（订阅源配置、平台规则、URL 前缀规则、系统配置）；
  - 数据转发热路径严格 100% 内存化。

### 阶段 8：Web 控制台深度进化 (接入面板、i18n 与移动端)
- [ ] **接入方式自动生成面板 (Platform Access Panel)**：
  - 输入业务账号、代理 Token 与目标网址，自动生成可复制的反代 URL、HTTP/SOCKS5 正代链接及完整 curl 示例；
  - 代理 Token 仅在浏览器本地 `localStorage` 保存，永不上传。
- [ ] **移动端自适应 (Mobile Responsive First)**：
  - 汉堡菜单收起抽屉、移动端卡片流式视图（Card View）、大触控热区。
- [ ] **全局中英文双语切换 (i18n)**：
  - 顶部导航栏一键 `中 / EN` 切换，本地持久化。
- [ ] **人性化细节**：
  - 延迟着色（≤400ms 绿、≤1000ms 黄、>1000ms 红）、地区码转国旗 Emoji、一键复制降级。

### 阶段 9：压测自动化脚本与性能基准报告
- [ ] **自动化压测套件 (`scripts/bench.sh`)**：
  - `bombardier` / `wrk` 高并发 QPS 压测与长连接压力测试。
- [ ] **火焰图 (Flamegraph) 诊断**：
  - 接入 `cargo-flamegraph` 定位 CPU 与内存热点，输出详细对比报告。
