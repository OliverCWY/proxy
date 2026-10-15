# Project Architecture Plan & Implementation Roadmap (AGENTS_PLAN.md)

本文件用于规划和追踪 Proxy Gateway 项目的阶段性演进目标、技术决策与实施路线。

---

## 阶段规划概览

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Proxy Gateway 演进路线图                         │
└────────────────────────────────────────────────────────────────────────┘
                                    │
    ┌───────────────────────────────┼──────────────────────────────┐
    ▼                               ▼                              ▼
【阶段 1：调度与数据面强化】     【阶段 2：控制面与订阅解析】    【阶段 3：Vue 3 + Bun 控制台】
 • 纯内存 LeaseTable            • Axum REST 控制面             • Bun + Vite + Vue 3 搭建
 • EgressIP O(1) 备用漂移       • 节点列表与单节点测活接口     • 实时指标看板 (Chart.js)
 • ArcSwap 前缀树 Trie 规则     • 规则沙箱测试接口 (/resolve)   • 规则调试器与抽屉管理
 • P2C + TD-EWMA 延迟衰减打分   • Base64 订阅拉取与调度        • rust-embed 单二进制融合
 ( [x] 已完成并提交 )           ( [x] 当前完成 )               ( [ ] 下一步实施 )
```

---

## 详细实施计划与当前进展

### 阶段 1：调度与数据面强化 (已完成并已独立 Commit)
- [x] **纯内存 Sticky Session 租约与平滑漂移 (`src/pool/lease.rs`)**：
  - 基于 `DashMap<AccountId, Lease>` 实现纯内存租约管理，零磁盘写盘，彻底切断 D 状态源头；
  - 维护 `EgressIpIndex` 反向索引，当原节点下线时以 $O(1)$ 时间复杂度快速找到同出口 IP 的备用节点，避免全表扫描；
  - 惰性过期与后台自动定期扫描淘汰。
- [x] **零侵入 URL 前缀 Trie 树 (`src/reverse/rules.rs`)**：
  - 实现最长前缀匹配树，Host 小写规范化、剥离端口与 IPv6 压缩、Path 严格区分大小写且剥 query；
  - 基于 `ArcSwap<AccountMatcher>` 实现无锁并发读与 Copy-On-Write 原子热更新。
- [x] **全链路测试覆盖 (`tests/gateway_v1.rs`)**：
  - 粘性会话与同 IP 备用节点漂移测试；
  - 前缀树规则最长匹配与 Headers 账号提取测试。

---

### 阶段 2：控制面 REST API 与订阅解析器 (当前阶段)
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

### 阶段 3：Vue 3 + Bun 现代化前端 Web UI 与单二进制打包 (下一步)
- [ ] **前端工程搭建 (Bun + Vite + Vue 3 + TypeScript + Tailwind)**：
  - 仪表盘看板（QPS、活跃连接、节点延迟分布）；
  - 节点列表管理（状态筛选、手动测活按钮与 Toast 回显）；
  - 规则调试器（输入测试 URL 与 Header，实时调试提取结果）。
- [ ] **单二进制融合 (`rust-embed`)**：
  - 将编译产物 `webui/dist` 嵌入 Rust 二进制文件；
  - 实现 SPA History Fallback（非资源路由全部回退到 `index.html`）；
  - 单个执行文件直接提供网关与控制台。
