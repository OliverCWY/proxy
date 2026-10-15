# Proxy Gateway Project Knowledge (AGENTS.md)

## 1. High-Level Map
- `src/inbound/`: 单端口嗅探与协议状态机（1 字节探测 0x05 SOCKS5 vs HTTP；HTTP 状态机区分 CONNECT 与 GET/POST 反代）。
- `src/outbound/`: 上游代理连接器（Direct、HTTP CONNECT、SOCKS5/SOCKS5h 本地与远端解析）。
- `src/pool/`: 无锁代理池（`ArcSwap<PoolSnapshot>` 热重载、P2C/RoundRobin 调度、指数退避熔断冷却、自动重试）。
- `src/reverse/`: HTTP 反向代理处理器（路径路由 `/target/...` 与 Host 头分流）。
- `src/forward.rs`: 正向代理数据面编排器（延迟 commit、数据双向透传）。
- `src/protocol/`: 强类型目标地址（`TargetAddr`）与鉴权凭据（`Credentials`）。
- `src/error.rs`: 强类型错误与 `is_retryable` 重试判定。

## 2. Invariants & Conventions
- **Commit Success 时序铁律**：必须且只能在 Upstream 连接建立完毕后，才向下游 Client 发送 200 Connection Established 或 SOCKS5 REP=0x00。
- **协议嗅探铁律**：Socket 首包严禁依靠多字节直接匹配协议前缀（因 TCP 无边界导致分包误判）。首字节只做 0x05 / 非 0x05 二分流，具体 HTTP 方法交由 HTTP 状态机读齐请求行再判定。
- **DNS 分离铁律**：上游 Proxy 服务器地址在入池前必须预解析为 `Arc<[SocketAddr]>`，避免拨号线程阻塞；目标网站地址（TargetAddr）在 SOCKS5h 下严禁在本地解析，必须保留域名传给远端。
- **内存安全**：禁止网络输入使用 `unsafe { str::from_utf8_unchecked }`。

## 3. Build & Test Commands
- 编译并运行：`cargo run`
- 运行全部测试：`cargo test`
- 运行网关核心测试：`cargo test --test gateway_v1 -- --nocapture`

## 4. Experiences Reference
- 深度网络协议踩坑与架构经验：[`./EXPERIENCES/PROTOCOL_GATEWAY.md`](./EXPERIENCES/PROTOCOL_GATEWAY.md)
- 追踪与待办计划：[`./AGENTS_TODO.md`](./AGENTS_TODO.md)
