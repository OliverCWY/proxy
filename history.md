AI：

### 一、当前已完成进展与底层技术收敛

你的端到端 MVP 已经走通了网络编程中最关键的“骨架”部分，底层设计思路已明确且完成收敛：

1. **Inbound 协议嗅探与单端口分流（已完成）**
   * 基于单端口实现首包探测，精准区分 SOCKS5（`0x05`）与 HTTP 请求（CONNECT / 绝对 URI / 相对 URI）。
2. **正向代理核心数据通路（已完成）**
   * **SOCKS5h 转发**：支持域名交由远端解析的完整 SOCKS5 握手。
   * **HTTP CONNECT 隧道**：解析正向连接请求并向对应目标建立双向盲转发通道。
3. **I/O 与内存管理机制优化（已收敛）**
   * **消除多余堆分配与中间包装**：握手阶段舍弃默认分配 8KB 堆内存的 `BufWriter`，改用栈上固定 Buffer 组包 + 单次 `write_all` 系统调用。
   * **原生 Socket 直通**：握手完成后直接还原并提取干净的裸 `TcpStream`，消除不必要的流包装状态机。

---

### 二、代理池完整 Feature 全景清单（按功能模块划分）

以下是参照 Resin 等生产级代理池网关，构建高并发、智能、可观测代理池所需实现的全部特性（分为 6 大子系统）：

```
                     ┌────────────────────────────────────────┐
                     │          代理池核心架构功能矩阵         │
                     └────────────────────────────────────────┘
                                         │
     ┌──────────────┬─────────────┼──────────────┬──────────────┬─────────────┐
     ▼              ▼             ▼              ▼              ▼             ▼
【协议接入】    【路由与粘性】  【节点韧性】   【转发引擎】   【控制与存储】  【可观测安全】
```

---

#### 1. 协议接入层（Inbound Subsystem）
* [x] **单端口多协议自动嗅探**：SOCKS5 与 HTTP 混合监听与分流。
* [ ] **URL 路径式反向代理（Axum 实现）**：
  * 支持 `http(s)://gateway:port/{user}/{pass}/{scheme}/{target}/{path}` 格式接入。
  * 自动将内部请求重写并转发至目标网站，解析目标响应并流式回传。
* [ ] **Caddy 前置集成**：
  * 对接 Caddy 终结 TLS、HTTP/2、HTTP/3（QUIC），将明文请求转发至 Rust 后端（支持 TCP 或 Unix Domain Socket）。
* [ ] **下游客户端身份认证（Auth）**：
  * SOCKS5 用户名/密码认证协议（RFC 1929）。
  * HTTP 正向代理 `Proxy-Authorization`（Basic 认证）。
  * 反向代理 Path 路径认证参数校验。

---

#### 2. 路由调度与粘性会话（Routing & Session Subsystem）
* [ ] **动态无锁节点池（`ArcSwap` 实现）**：
  * 承载海量可用节点（如数万个），支持并发读与瞬时全量原子热替换。
* [ ] **会话粘性租约管理（Sticky Session / `DashMap` 实现）**：
  * 支持基于业务账号（Account）或客户端 IP 的租约绑定。
  * 在指定 TTL 时间窗口内（例如 10 分钟），同一用户的请求严格路由至同一个上游代理出口。
* [ ] **智能负载均衡选路算法**：
  * **P2C（Power of Two Choices）**：随机挑选两个候选节点，择优录取。
  * **TD-EWMA（时间衰减指数加权移动平均）**：基于历史 RTT 延迟与惩罚机制计算节点综合得分。
  * **标签定向路由**：支持指定协议、国家/地区（GeoIP）、提供商等标签进行节点过滤。

---

#### 3. 节点健康度与容灾机制（Resilience & Lifecycle Subsystem）
* [ ] **主动健康检查（Active Prober）**：
  * 后台轻量协程池，定期使用上游代理请求探针地址（如 `cp.cloudflare.com` 或 `httpbin.org/ip`），校验连通性与真实出口 IP。
* [ ] **被动熔断机制（Passive Circuit Breaker）**：
  * 在实际转发过程中监控连接超时、网络重置（RST）、EOF 等错误；当节点连续失败达到阈值时触发熔断。
* [ ] **指数退避冷却池（Cooldown Pool）**：
  * 熔断节点移入隔离池，暂停接收线上流量；冷却时间按失败次数指数级递增，空闲测活成功后自动升格放行。
* [ ] **概率黑名单过滤器（Blacklist Filter）**：
  * **布谷鸟过滤器（Cuckoo Filter）** 或 **RoaringBitmap**：超低内存占用、支持动态剔除（节点恢复后洗白）的判重结构。
  * 对彻底失效或被目标站封禁的节点实施快速拦截，免去哈希表全量字符串存储。

---

#### 4. 上游出站与转发引擎（Outbound & Forwarding Subsystem）
* [ ] **多协议上游统一抽象（Outbound Connector）**：
  * 抹平上游 HTTP CONNECT 代理与 SOCKS5 代理的差异，对外暴露统一的拨号接口。
* [ ] **上游 DNS 预解析与缓存**：
  * 节点载入时将 Host 预先解析为 `Vec<SocketAddr>`，彻底规避每次发起请求时的 DNS 阻塞。
* [ ] **全局单实例 HTTP 转发客户端（针对反向代理）**：
  * 配合 `hyper-util` 与自定义 Connector，实现全进程复用单个 Client，通过 Request Context 动态绑定不同的上游代理。
* [ ] **数据转发通道双模式**：
  * **便携用户态模式**：`tokio::io::copy_bidirectional`（跨平台通用基线）。
  * **内核态零拷贝模式（Linux 专享）**：`splice(2)` 双向管道桥接，彻底 bypass 用户态内存拷贝。

---

#### 5. 控制面与持久化（Control Plane & Persistence Subsystem）
* [ ] **外部订阅源拉取与同步**：
  * 支持配置多个远程订阅 URL，定时拉取节点并自动去重、格式化、入池。
* [ ] **管理 REST API（Axum 实现）**：
  * 节点池状态查询（在线数、失败率分布）。
  * 动态增删节点与订阅源。
  * 手动触发测活、清空黑名单、重置租约。
* [ ] **轻量级持久化引擎（Turso / libSQL）**：
  * 纯冷数据持久化：用户鉴权列表、静态配置、黑名单种子、持久化订阅源。
  * 严格隔离：热路径转发与瞬时指标绝不触碰磁盘数据库。

---

#### 6. 可观测性与安全加固（Observability & Security Subsystem）
* [ ] **Prometheus 兼容指标收集**：
  * 全局 QPS、活跃 TCP 连接数、节点响应耗时直方图（Histogram）、出入站流量统计。
* [ ] **防 D 状态卡死的异步背压日志**：
  * 使用有界通道（Bounded Channel）剥离日志处理；若磁盘写入拥堵触发背压，自动丢弃日志（Drop on full），保护网络主协程。
* [ ] **HTTP 走私与畸形报文防御**：
  * 校验并拒绝重复 `Content-Length`、畸形 `Transfer-Encoding` 等攻击性报文。
* [ ] **防 Slowloris / 慢速连接攻击防护**：
  * 为下游握手、上游建立连接、初次读取全流程配置严格的空闲超时与全局超时。

------

上面是其他 AI 的总结。差不多用 bufreader（预期MVP先把残余 buffer写给上游，再 into_inner，避免中转） 和 bufwriter(flush之后直接into_inner) 写完了上下游（http只支持connect，socks默认socks5h）正向代理连接。我是初级 mlir cpp 开发为工作，想补课网络和系统开发。基于现在的进度端到端MVP后，下一步优先哪个：
1. 继续做代理池的功能：实现鉴权，反向代理，冷却，测活，拉黑功能 -> 进阶：多账户/平台+出口探测+粘性+测延迟+测网速
2. 用 bytemut + tcpstream 自己包装一个 BufStream （这样更能搞懂底层细节）/直接返回（Bytes+TcpStream）或用tokio-utils/直接peek实现用户态零拷贝，还有上游代理 host 从 String 转变为预处理/oncelock（这里由于用的 arc-swap，更新应该用新vec？提取目标信息，是用tokio还是其他社区方案？） 变成 [SocketAddr] (即 TcpStream 接受的类型，不再需要 dns 解析）；host/user/pass 是用原始 bytes 还是单独复制一份？应该单独复制最合适吧？不过这些所有权唯一，不需要修改，可以 Box<[u8]>，是不是没必要用 bytes？还是说bytes其实基本上全方面优于 box？不过我现在用的是 SmallVec，长度上限给到32，感觉拷贝代价变大了
3. 将现有的手动协议解析全换成社区的 zero-copy 方案
4. 用社区高性能零拷贝方案解析协议/握手，但协议是我自己规定的（如num/nom/叫啥的来着？或者 pest）（我自己之前手写过 lua parser，但工作中或者自己没怎么用框架写过）
5. 认证/更新节点列表/更新配置等，分别专门交给一个线程处理，还是每个线程自己处理自己的？
6. inbound 应该配置两个步骤：握手+获取 auth，以及取得 target
7. 目前感觉代码还是有些乱，该如何整理？滥用的 invalid_data 需要我继续花精力解决，还是先这样？

额外备注：
1. Cuckoo Filter(我知道假阳性的代价） / RoaringBitmap 是我本身就对这几个算法感兴趣，想玩玩看
2. 健康/冷却/拉黑节点有必要分开存吗？即健康/冷却/拉黑节点分开存储
3. 如果存在一起，状态可否使用 AtomicEnum?
4. 反代应该只和正代共享嗅探，其他流程完全独立
5. 破坏性（内存）拉黑或代理原链接落盘也是我目前 desired 设计，因为之前遇到过 resin D 状态的坑

------
AI：

我按你 GitHub **公开 `main` 当前版本**来评；你本地如果已经把 `BufWriter`/残余 buffer 那部分改掉了，那一两条可以忽略。仓库现在只有很薄的一层代码，所以反而很适合现在把边界修正，不要等代理池长大后再动骨架。([GitHub](https://github.com/OliverCWY/proxy))

## 先给结论

**下一步不要优先做 2/3/4 的“更底层/更 zero-copy”。**

你的优先级应该是：

> **协议语义修正 + 状态机定型 → typed error → 测试 → hard-code 鉴权 → retry → 最小代理池 lifecycle → ArcSwap/control plane → 反代 → 高级调度 → 最后才是零拷贝/自制 BufStream。**

换成你列的编号，大概是：

**6 → 7 → 1 的鉴权/冷却/测活部分 → retry → 5 → 1 的粘性/P2C/EWMA → 反代 → 3/4 → 2 的自制 BufStream/splice。**

其中 **2 可以单独开 learning branch 玩**，但我不建议现在让生产主线依赖它。

严格按“能上线当生产代理网关”的标准，我会给当前代码 **3~4/10**；按“四天写的学习型网络 MVP”，则是合理的实验代码。真正拉低评分的不是代码丑，而是现在有几处 **protocol correctness / safety 级别的问题**，而你花了比较多精力在 SmallVec、BufWriter、零拷贝这些二级问题上。

---

# 一、目前最需要修的 anti-pattern

## P0：不是代码风格问题，是协议语义问题

### 1. 你现在“成功回复”发得太早

这是当前最大问题。

HTTP CONNECT 的 `parse_http()` 在读到空行后直接：

```rust
HTTP/1.1 200 Connection Established
```

但这时你甚至还没有 `proxy.connect(&target)`。SOCKS5 也是在 `handle_socks5_request()` 里直接回 `REP=0x00`，之后才真正连接上游。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/inbound.rs))

这会导致：

```text
Client
  │
  │ CONNECT google.com:443
  ▼
Gateway
  │──── 200 OK ────► Client      ← 已经向客户端 commit 成功
  │
  │ connect upstream
  ▼
失败
```

此时 **retry 已经基本没法正确实现**。

RFC 9110 明确是成功 CONNECT 后才切换到 tunnel；非成功响应意味着 tunnel 尚未建立。SOCKS5 的 CONNECT reply 同理，是服务器 evaluate request 后给出结果。([RFC Editor](https://www.rfc-editor.org/rfc/rfc9110.html?utm_source=chatgpt.com))

所以你现在最重要的接口应该变成：

```text
parse/auth/target
        ↓
select proxy
        ↓
connect upstream
        ↓
retry if necessary
        ↓
send success reply
        ↓
forward buffered residual
        ↓
raw tunnel
```

**这也是为什么我支持你第 6 点，但要再细一点。**

建议 inbound 生命周期：

```text
sniff
  ↓
protocol negotiation
  ↓
authentication
  ↓
parse target
  ↓
authorization / routing
  ↓
dial + retry
  ↓
commit_success()
  ↓
tunnel
```

其中 SOCKS negotiation 本身必然包含写操作，所以不要试图把它设计成纯 parser。

---

### 2. SOCKS5 method negotiation 现在违反协议

目前：

```rust
let supports_auth = ...
if requires_auth {
    reader.write_all(&[0x05, 0x02]).await?;
    if !supports_auth { ... }
} else {
    reader.write_all(&[0x05, 0x00]).await?;
}
```

也就是说不需要认证时，即使客户端**没有提供 method 0x00**，你还是选 `0x00`；需要认证时也是先选 `0x02`，再判断客户端有没有提供。

SOCKS5 要求服务器必须从客户端 `METHODS` 中选择；没有可接受的就返回 `0xff`。([RFC Editor](https://www.rfc-editor.org/info/rfc1928/?utm_source=chatgpt.com))

另外：

```rust
buf.into_iter().any(...)
```

应该是：

```rust
buf[..nmtd].contains(...)
```

现在找 `0x02` 暂时不会因为尾部全零出 bug，但这个代码写法会非常容易在以后检查 no-auth 时踩坑。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/inbound.rs))

---

### 3. SOCKS 成功响应的 BND.ADDR/BND.PORT 也不对

现在基本上是在把客户端提交的 target 地址重新 echo 回去。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/inbound.rs))

但 SOCKS5 CONNECT response 里的：

```text
BND.ADDR
BND.PORT
```

表示的是服务器为目标连接所使用/绑定的地址，不是 DST.ADDR。RFC 1928 对此有明确语义。([RFC Editor](https://www.rfc-editor.org/info/rfc1928/?utm_source=chatgpt.com))

MVP 可以简单处理，但不要假装它等于 target。

---

### 4. `peek(8)` 这个 sniff 有经典 TCP fragmentation bug

你：

```rust
let mut buf = [0u8; 8];
client.peek(&mut buf).await?;
...
b'C' if buf.starts_with(b"CONNECT ")
```

但是你**完全没检查 peek 返回的 n**。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/inbound.rs))

TCP 没有消息边界。

客户端完全可以发生：

```text
packet/read 1: "C"
packet/read 2: "ONNECT "
```

Tokio `peek()` 只保证返回当前可用的 `n` 个字节，并没有说填满你的 8 字节 buffer。([Docs.rs](https://docs.rs/tokio/latest/tokio/net/struct.TcpStream.html?utm_source=chatgpt.com))

于是第一次得到：

```text
['C', 0, 0, 0, 0, 0, 0, 0]
```

你的程序会把它分类成 reverse。

### 最简单的修法反而更漂亮：

只 peek **1 byte**：

```text
0x05 → SOCKS5
其他 → HTTP
```

然后 HTTP 层自己判断：

```text
method == CONNECT → forward proxy
otherwise         → reverse proxy
```

这也是未来把 HTTP 交给 Hyper 最自然的结构。

---

### 5. `from_utf8_unchecked()` 必须现在删掉

你这里：

```rust
unsafe { str::from_utf8_unchecked(domain) }
```

domain 又有可能来自网络输入。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/mod.rs))

这是我在整个仓库里最不喜欢的一行。

`from_utf8_unchecked` 的 safety contract 就是输入**必须已经保证是有效 UTF-8**。([Rust Documentation](https://doc.rust-lang.org/std/str/fn.from_utf8_unchecked.html?utm_source=chatgpt.com))

这里没有这个 invariant。

这不是“unsafe 但性能好”，而是 **unsafe 没有证明**。

直接：

```rust
str::from_utf8(domain)?
```

或者更合理：

```rust
Host::Domain(Box<str>)
```

在构造 `Host` 时就完成 validation。

这个检查只做一次，性能完全不值得拿 UB 风险换。

---

# 二、HTTP parser 还有几个比较重要的问题

公开代码里的 HTTP CONNECT parser 目前还是“够 demo，不够协议实现”。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/inbound.rs))

比较明显的有：

- `Proxy-Authorization` header name 被当成大小写敏感；
- `Basic ` 也被大小写敏感处理；
- CONNECT request-line 没真正验证 method/version；
- IPv6 CONNECT authority 没处理；
- header 单行没有真正的预分配上限；
- `read_until('\n')` 是**先增长 Vec 再检查总长度**，所以攻击者可以先让你分配很大内存；
- header limit 现在是 1 MiB，对一个 CONNECT handshake 太宽；
- parser 和 response writer 耦在一起，使 retry / auth rejection / error mapping 很难做。

Basic proxy auth 应该在失败时走 `407 Proxy Authentication Required` + `Proxy-Authenticate`；RFC 7617 也明确说明了 proxy authentication 的对应行为。([RFC Editor](https://www.rfc-editor.org/info/rfc7617/?utm_source=chatgpt.com))

**这里我建议你直接用 `httparse`。**

它现在就是一个非常小的 HTTP/1.x push parser，目标就是 fast/safe/zero-copy。([Docs.rs](https://docs.rs/httparse/latest/httparse/index.html?utm_source=chatgpt.com))

但只把：

```text
HTTP header syntax parsing
```

交给它。

你自己的业务仍负责：

```text
CONNECT?
authority 合法？
认证？
是否允许目标端口？
route 到哪个 proxy？
什么时候回复 200？
```

---

# 三、Outbound 现在也有几个值得马上修的地方

## 1. HTTP response 一字节一字节读，不值得

现在：

```rust
while stream.read_exact(&mut byte).await.is_ok() {
    resp_buf.push(byte[0]);
    ...
}
```

([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/outbound.rs))

这是为了避免 over-read，我理解你的动机，但会产生大量小 read。

更严重一点的是：如果在 `\r\n\r\n` 前遇到 EOF/error，循环直接退出，之后你仍然继续解析已有的 `resp_buf`。

然后：

```rust
first_line.windows(3).any(|w| w == b"200")
```

也不是真正的 status-code parser。

例如理论上：

```text
HTTP/1.1 500 something200
```

也可能被当成成功。

CONNECT 成功其实应接受合法的 **任意 2xx**。([RFC Editor](https://www.rfc-editor.org/rfc/rfc9110.html?utm_source=chatgpt.com))

还是用 `httparse::Response`。

读一个 1~4K 的 bounded buffer：

```text
read
  ↓
httparse
  ↓
Partial → 再读
Complete → 找 header_end
  ↓
header 后面的 bytes 保留下来
```

然后把残余 bytes 发给另一端，再还原裸 `TcpStream`。

这个地方 `BytesMut` 很适合。

---

## 2. `dns: bool` 现在其实没有语义

`Socks5 { dns, ... }` 最后：

```rust
dns: _dns
```

没有使用。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/outbound.rs))

所以当前：

```text
socks5
socks5h
```

实际上没有区别。

建议不要叫 `bool`：

```rust
enum DnsMode {
    Local,
    Remote,
}
```

然后行为必须非常明确。

---

# 四、我认为你代码里最大的设计问题：`Endpoint` 混了两种完全不同的东西

这是比 BufStream 更值得你现在重构的一点。

你现在的 `Endpoint` 同时表示：

1. **上游代理服务器地址**
2. **最终目标网站地址**

但它们的 DNS semantics 完全不同。

### 上游 proxy host

例如：

```text
proxy.example.com:1080
```

这是 gateway 自己必须连接的东西。

你完全可以：

```text
订阅载入
    ↓
DNS resolve
    ↓
Arc<[SocketAddr]>
```

然后热路径直接：

```rust
TcpStream::connect(&node.addrs[..])
```

这是你说的预解析，**我赞成**。

目前 `get_socket_addr()` 每连接一次都 `lookup_host`，甚至 IP 地址也经过 `lookup_host()`，还新建 `Vec<SocketAddr>`。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/mod.rs))

这是应该消掉的。

### 但目标网站 host 不一样

比如：

```text
example.com:443
```

如果你的上游是 SOCKS5h：

> **绝不能提前在 gateway 本地 resolve。**

因为 remote DNS 本来就是 socks5h 的重要语义。

所以我建议拆：

```rust
struct TargetAddr {
    host: TargetHost,
    port: u16,
}

enum TargetHost {
    Ip(IpAddr),
    Domain(Box<str>),
}

struct ProxyNode {
    id: NodeId,
    dial_addrs: Arc<[SocketAddr]>,
    protocol: ProxyProtocol,
    ...
}
```

这样 DNS 语义立刻清楚很多。

---

# 五、`OnceLock` 不适合上游 DNS

你问：

> host 从 String 转预处理 / OnceLock，配 ArcSwap 更新是不是新 vec？

**用新 snapshot，不用 OnceLock。**

`OnceLock` 的语义是“一旦初始化以后永远不改”。

但 DNS 天生有：

```text
TTL
地址变化
provider 迁移
订阅更新
```

你准备 ArcSwap，本来就是正确方向。

例如：

```text
ArcSwap<PoolSnapshot>

PoolSnapshot {
    nodes: Arc<[Arc<Node>]>
}
```

control plane：

```text
download config
→ parse
→ resolve upstream hosts
→ build complete new snapshot
→ pool.store(new_snapshot)
```

请求处理：

```rust
let snapshot = pool.load();
```

整个连接使用同一代 snapshot。

ArcSwap 本来就特别适合这种“routing table / config read-mostly、偶尔全量替换”的场景，其 read path 是 lock-free 的。([Docs.rs](https://docs.rs/arc-swap/latest/arc_swap/?search=std%3A%3Avec&utm_source=chatgpt.com))

---

# 六、`Box<[u8]>`、`Bytes`、SmallVec：你的直觉需要微调一下

## `Bytes` 并不是全方面优于 `Box`

`Bytes` 的核心优势是：

- cheap clone；
- 多 owner；
- O(1) slicing；
- 多个 view 指向同一块 buffer。([shadow.github.io](https://shadow.github.io/docs/rust/bytes/struct.Bytes.html?utm_source=chatgpt.com))

如果你的数据：

```text
唯一所有权
构造一次
永远不修改
不需要切片共享
```

那我反而推荐：

```rust
Box<[u8]>
Box<str>
```

它的语义更准确。

### 我会这样选

| 数据 | 建议 |
|---|---|
| proxy domain | `Box<str>` |
| proxy 预计算 auth wire/header | `Box<[u8]>` |
| `ArcSwap` snapshot 中多个地方共享 node | `Arc<Node>` |
| HTTP receive/parse buffer | `BytesMut` |
| 从一个 parse buffer 创建多个 O(1) slice | `Bytes` |
| 临时构造十几/几十 byte handshake | 小容量 `SmallVec` / stack array |

---

## 你现在真正过头的是 `SmallVec<[u8; 512]>`

你现在：

```rust
pub type Auth =
    SmallVec<[u8; MAX_EXPECTED_AUTH_HEADER_LENGTH]>;
```

其中 capacity 是 512。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/outbound.rs))

这意味着 inline capacity 本身就是结构的一部分。

如果未来你有：

```text
30,000 proxies
```

这种设计会让节点对象非常胖，即使绝大部分 auth 实际只有几十字节。

甚至因为它在 enum variant 里，整个 `SimpleProxy` enum 的尺寸也要容纳最大 variant。

我会把这一项直接换：

```rust
Box<[u8]>
```

预计算一次就结束。

至于：

```rust
SmallVec<[u8; 32]>
```

你担心“拷贝代价大了”——我不会担心。

移动几十个 byte 和一次 heap alloc 相比根本不是你当前系统的主要成本。

---

# 七、domain 上限 64 也应该改

你现在：

```rust
MAX_EXPECTED_DOMAIN_SIZE = 64
```

([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/mod.rs))

这很像是把：

> DNS 单个 label 最大 63 bytes

和：

> 整个 hostname

混起来了。

SOCKS5 ATYP=DOMAIN 前面本身就是一个 1-byte length，所以 wire protocol 支持最多 255 octets。([RFC Editor](https://www.rfc-editor.org/info/rfc1928/?utm_source=chatgpt.com))

因此 `HostContainer<[u8;64]>` 不应该成为 protocol limit。

---

# 八、是否应该自己写 `BufStream`？

## 作为学习：应该。

## 作为你现在主线的下一个 feature：不应该。

这是两个答案。

你本来就是做 MLIR/C++，想补系统开发，所以自己实现一次：

```text
buffered AsyncRead
partial frame
read cursor
write buffering
flush semantics
EOF
backpressure
```

学习价值很大。

甚至我建议你单独写一个小 crate / branch：

```text
toy-bufstream
```

加几十个测试。

但代理主线现在完全不需要它。

你描述的方案：

> BufReader 中剩余内容先写给上游，然后 `into_inner()`

其实已经很好。

Tokio 自己也明确提示：直接 `into_inner()` 会丢掉 buffer 里未消费的数据。([Docs.rs](https://docs.rs/tokio/latest/%20tokio/io/struct.BufReader.html?utm_source=chatgpt.com))

所以：

```rust
outbound.write_all(reader.buffer()).await?;
let client = reader.into_inner();
```

这种生命周期清晰得多。

同理 BufWriter：

```text
flush
→ into_inner
```

即可。

公开 main 里 SOCKS request 目前仍有默认 `BufWriter`，而 Tokio 默认容量当前是 8 KiB。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/inbound.rs))

你本地如果已经删了，那就是正确方向。

---

# 九、`peek` 不是“用户态零拷贝”

这个概念建议现在就纠正。

`MSG_PEEK` 是：

> 从 socket receive queue 把数据返回到你的 user buffer，但不从队列移除。

后面真正 `read()` 时还会再次获得这部分数据。([man7.org](https://man7.org/linux/man-pages/man2/recv.2.html?utm_source=chatgpt.com))

所以：

```text
peek
```

解决的是：

```text
don't consume
```

不是：

```text
zero-copy
```

如果 Linux 下真想绕用户态 copy，才是你后面想玩的：

```text
splice()
```

Linux 的 `splice` 可以通过 pipe buffer/page reference 避免通常的数据页复制。([man7.org](https://man7.org/linux/man-pages/man2/splice.2.html?utm_source=chatgpt.com))

但甚至现在的 Rust async `splice` 实现自己都明确提醒：小块传输/频繁 syscall 时可能**反而比正常 read/write 慢**。([Docs.rs](https://docs.rs/tokio-splice2/latest/tokio_splice2/?utm_source=chatgpt.com))

所以这绝对应该放在 benchmark 之后。

Tokio 的 `copy_bidirectional()` 默认只是两边各一个 8 KiB buffer，这已经是很好的 baseline。([Docs.rs](https://docs.rs/tokio/latest/src/tokio/io/util/copy_bidirectional.rs.html?utm_source=chatgpt.com))

---

# 十、是否把手写 parser 全换社区 zero-copy？

答案是：

### HTTP：换。

### SOCKS5：不要。

### 自定义协议：可以用 parser combinator 学习。

SOCKS5 本质就是这种东西：

```text
read u8
read u8
read N
match ATYP
read N
```

手写状态机会比 nom 清楚得多。

而且 SOCKS 协议难点根本不是 parse bytes，是：

```text
method negotiation
auth
request
dial
reply mapping
retry
state transition
```

parser framework 帮不了你这些。

---

# 十一、自定义协议用什么？你记得的是 `nom`

你说的“num/nom/叫啥的”是 **`nom`**。

现在 nom 仍支持：

- byte slice；
- zero-copy slice output；
- streaming / partial input；
- custom error type。([GitHub](https://github.com/rust-bakery/nom?utm_source=chatgpt.com))

另一个很值得你试的是 **`winnow`**，它原生支持 `&[u8]`、`Bytes` 和 partial stream。([Docs.rs](https://docs.rs/winnow/latest/winnow/stream/index.html?utm_source=chatgpt.com))

我个人给你这个项目的选择：

```text
标准 HTTP/1       → httparse
SOCKS5            → 手写
你的 binary proto → nom / winnow
stream framing    → tokio-util::codec
pest              → 不推荐这里用
```

Pest 更适合 grammar/text-oriented parser，你这个是高频 binary/network protocol，不是它最舒服的领域。

---

# 十二、第 5 个问题：每个 subsystem 专门一个线程吗？

**不要用“线程”设计这个系统。**

先用：

```text
async tasks + immutable snapshot + channel
```

来思考。

我会这么分：

```text
                ┌──────── Control task ────────┐
subscription ──►│ parse / resolve / validate   │
admin API ─────►│ build PoolSnapshot           │
                └───────────┬──────────────────┘
                            │ ArcSwap::store
                            ▼
                   ArcSwap<PoolSnapshot>
                            │
           ┌────────────────┼─────────────────┐
           ▼                ▼                 ▼
       conn task         conn task         conn task
```

### Auth

每个 connection task **自己查 immutable auth snapshot**。

不要：

```text
每个请求 → auth thread → reply
```

这会人为建立一个串行中心。

只有 Argon2 这种 CPU-hard password hash 才应该：

```text
spawn_blocking + semaphore
```

你的 proxy auth MVP 直接 hard-code byte comparison 完全合理。

### 节点更新 / config update

一个逻辑上的 single writer task 非常好。

但不需要 dedicated OS thread。

### Logging / disk persistence

这里反而应该真正隔离。

```text
network task
    │ try_send
    ▼
bounded channel
    │
blocking worker / logging process
```

full 就 drop/coalesce。

你碰过 Resin 的 D state，这个担心是合理的：**绝对不要让 data plane await 普通磁盘落盘完成**。

---

# 十三、健康 / 冷却 / 黑名单该不该分开存？

语义上分开。

对象本身不要复制到三个 registry。

我建议：

```text
Canonical Node Registry
         │
         ├── healthy eligible snapshot
         │
         ├── cooldown timer/index
         │
         └── blacklist index
```

比如：

```rust
struct Node {
    id: NodeId,
    config: NodeConfig,
    runtime: NodeRuntime,
}
```

状态：

```text
Healthy
Suspect
Cooldown
HalfOpen
Disabled
```

而：

```text
Blacklisted
```

我甚至倾向于把它当成**policy**，而不是 health。

因为：

```text
timeout 3 次 → Cooldown
```

和：

```text
目标站封了这个出口
管理员永久禁掉
订阅源已删除
```

根本不是一类东西。

---

# 十四、能不能 AtomicEnum？

标准库没有一个你直接拿来用的 `AtomicEnum`。

常见做法：

```rust
#[repr(u8)]
enum State {
    Healthy = 0,
    Cooldown = 1,
    HalfOpen = 2,
}

AtomicU8
```

需要 transition 时 `compare_exchange()`。Rust 自己提供的是 `AtomicU8` 这种原子整数。([Rust Documentation](https://doc.rust-lang.org/nightly/core/sync/atomic/type.AtomicU8.html?utm_source=chatgpt.com))

但我建议你**先别用它**。

因为很快你就会发现状态不只有一个 enum：

```text
state
failure_count
cooldown_until
last_failure
last_latency
half_open_inflight
```

六个 atomic 很容易出现逻辑上互相不一致。

更简单的设计：

```rust
Mutex<NodeRuntimeState>
```

state transition 又不是百万 QPS hot path。

而 selector 的 hot path 直接读：

```text
ArcSwap<HealthySnapshot>
```

根本不需要碰 mutex。

先把正确性做出来，再根据 profiling 决定是不是 AtomicU8。

---

# 十五、Cuckoo Filter / RoaringBitmap

可以玩，而且挺适合你这个项目当算法练习，但**不要让它成为 MVP dependency**。

还有一个概念：

**RoaringBitmap 不是概率数据结构。**

它是压缩整数 bitmap，membership 是 exact。

所以如果你的 node 已有稳定：

```rust
NodeId: u32
```

RoaringBitmap 非常适合：

```text
blacklisted NodeId set
```

尤其目标站 blacklist：

```text
target A → bitmap<NodeId>
target B → bitmap<NodeId>
```

会挺有意思。

Cuckoo Filter 则适合：

```text
海量 key
愿意接受 false positive
要求支持删除
```

但几万代理节点其实 `HashSet<NodeId>` 都够快够小。

所以我会：

```text
主实现：HashSet / bitmap
实验 branch：Cuckoo Filter
```

别为了算法兴趣污染核心状态机。

---

# 十六、反代和正代是不是只共享 sniff？

我会稍微修正你的说法：

**协议处理流程应该完全独立，但基础设施应该大量共享。**

共享：

```text
listener / admission
timeouts
Auth backend
ProxyPool
Node lifecycle
Outbound Connector
Routing
Metrics
Config
```

不共享：

```text
HTTP reverse request processing
CONNECT tunneling
SOCKS handshake
stream relay
```

理想结构：

```text
                     Listener
                        │
                peek first byte
                 /             \
              0x05            HTTP
               │                │
            SOCKS5        Hyper/Axum service
                                │
                      ┌─────────┴─────────┐
                    CONNECT            other
                      │                  │
                ForwardProxy       ReverseProxy
```

这样最干净。

---

# 十七、你现在的 `invalid_data()` 要不要继续改？

**要，而且优先级很高。**

但不是继续优化 `invalid_data()`。

而是**删掉“所有东西都是 InvalidData”这个模型。**

你 Cargo 已经依赖了 `thiserror`，正好用起来。([GitHub](https://github.com/OliverCWY/proxy/blob/main/Cargo.toml))

至少做：

```rust
enum InboundError {
    Io(io::Error),
    MalformedRequest,
    UnsupportedMethod,
    UnsupportedCommand,
    AuthRequired,
    AuthFailed,
    Timeout(Stage),
}

enum ConnectError {
    Resolve(...),
    Connect(...),
    Timeout(Stage),
    UpstreamAuthFailed,
    UpstreamRejected(ReplyCode),
    UpstreamProtocol(...),
}
```

然后：

```rust
impl ConnectError {
    fn retryable(&self) -> bool
}
```

这一步必须放到 retry 前。

否则你后面没法区分：

```text
connection refused    → retry another proxy
proxy auth failed     → 不应该一直 retry
malformed client req  → 更不应该 retry
target forbidden      → policy error
timeout               → health penalty
```

`anyhow` 留到 `main()` / task 顶层做 error reporting 就好。

---

# 十八、Retry 应该比代理池高级功能更早实现

你现在列“没实现重试”，我认为它应该提前。

但 retry 必须有一个铁规则：

> **只在向下游 commit success 之前 retry。**

例如：

```text
client CONNECT
     ↓
proxy A connect timeout
     ↓
mark failure
     ↓
proxy B connect
     ↓
success
     ↓
send client 200
     ↓
tunnel
```

很好。

但是：

```text
send client 200
↓
已经转发 TLS ClientHello
↓
proxy 挂了
```

这时候绝不能神奇切 proxy B。

因为 arbitrary TCP byte stream 通常不可 replay。

所以你修完“成功 reply 太早”以后，retry 会变得非常自然。

---

# 十九、我会给你一个非常具体的 Roadmap

## M0 — Protocol correctness freeze

先不加 feature。

完成：

```text
□ 去掉 from_utf8_unchecked
□ 修 SOCKS method selection
□ SOCKS RFC1929 hard-code auth
□ HTTP Basic hard-code auth
□ 认证失败正确 reply
□ CONNECT/SOCKS success 延迟到 outbound 成功后
□ SOCKS reply 正确 REP mapping
□ sniff 不依赖一次 peek 8 bytes
□ HTTP response 正确 parse 2xx
□ dns mode 真正有语义
□ IPv6 CONNECT authority
□ handshake / dial timeout
□ typed errors
```

同时写 integration tests。

尤其测试 TCP segmentation：

```text
"C"
"ON"
"NECT "
```

以及 SOCKS 每一个 byte 单独发送。

如果你的 parser 经得住这个，才算网络 parser。

---

## M1 — 固化核心接口

把当前 `Endpoint` 拆掉：

```text
TargetAddr
ProxyNode
ResolvedProxyEndpoint
```

Outbound 不需要现在做复杂 trait hierarchy。

你目前所有 `SimpleProxy` variant 最终都返回 `TcpStream`，但为了抽象已经每连接做了一次 `Box<dyn IoStream>`。([GitHub](https://raw.githubusercontent.com/OliverCWY/proxy/main/src/proxies/outbound.rs))

我会先：

```rust
enum Outbound {
    Direct,
    Http(...),
    Socks5(...),
}

impl Outbound {
    async fn connect(...) -> Result<TcpStream, ConnectError>
}
```

**enum dispatch 比 dyn trait 更适合你现在。**

以后真出现：

```text
TLS stream
QUIC stream
yamux stream
```

再抽 `dyn AsyncRead + AsyncWrite`。

这也是你现在“接口设计花时间很多”的一个来源：抽象发生得太早。

---

## M2 — Pool v1

只做：

```text
ArcSwap<PoolSnapshot>
RoundRobin / Random
NodeId
basic failure counter
retry 2~3 candidates
```

不要 EWMA。

不要 Sticky。

不要 GeoIP。

不要测速。

先让：

```text
10k nodes
hot replace
concurrent selection
retry
```

跑稳。

---

## M3 — Lifecycle

然后上：

```text
passive failures
↓
Healthy → Cooldown
↓
exponential backoff
↓
active probe
↓
HalfOpen
↓
Healthy
```

这里你会真正学到：

```text
state machine
timer scheduling
concurrency
backpressure
```

对你“补系统开发”的学习收益比自己写 BufStream 高很多。

---

## M4 — Control plane

再加：

```text
subscription pull
DNS preprocessing
config reload
admin API
auth snapshot
persistence
```

一个 writer 建新 snapshot，然后 ArcSwap 替换。

这一步以后你的架构就基本成型了。

---

## M5 — Sticky + P2C

先 Sticky：

```text
AccountId → Lease(NodeId, expiry)
```

然后 P2C。

再加最简单 latency EWMA。

最后才 TD-EWMA / penalty。

否则你会在一个没稳定的 node lifecycle 上调 scoring formula，没有意义。

---

## M6 — Reverse proxy

到这里再做 Axum/Hyper。

因为届时 reverse proxy 只需要：

```text
Request
 ↓
authenticate
 ↓
route
 ↓
Pool.select()
 ↓
HTTP Client using selected connector
```

核心 proxy pool 不需要为 reverse proxy 改架构。

---

## M7 — 性能阶段

先 benchmark：

```text
connections/s
handshake latency
RSS / connection
throughput
syscalls
allocations
```

再决定：

```text
BytesMut parser
vectored IO
custom buffer
copy_bidirectional buffer tuning
splice
io_uring
```

Linux `splice` 可以做 A/B benchmark，不要默认它一定赢。([Docs.rs](https://docs.rs/tokio-splice2/latest/tokio_splice2/?utm_source=chatgpt.com))

---

# 二十、如果我是你，明天打开代码第一件事会做什么

不是 ArcSwap。

不是 Cuckoo。

不是 Bytes。

不是反代。

我会把主连接流程改成这个：

```text
accept
  ↓
sniff protocol
  ↓
InboundSession::negotiate()
  ↓
credentials + TargetAddr
  ↓
auth.validate()
  ↓
pool.connect_with_retry(target)
  ↓
InboundSession::reply_success()
  ↓
forward prefetched bytes
  ↓
TcpStream + TcpStream
  ↓
copy_bidirectional
```

然后为这个流程写十几个 integration test。

**一旦这个骨架定下来，你后面 80% 的 feature 都只是往里面插模块。**

你现在觉得“接口怎么设计都不满意”，很大程度不是 Rust 熟练度的问题，而是现在抽象围绕的是：

```text
BufReader / BufWriter / SmallVec / trait object
```

这些实现细节。

你真正应该让接口围绕的是：

```text
Session
Target
Credential
Node
Dial
Commit
Retry
Lifecycle
```

把这一层设计对以后，`Box<[u8]>` 还是 `Bytes` 反而只是局部实现选择。

最后一句最重要：**你当前最值得学习的“底层”不是再少一次 memcpy，而是 TCP 没有消息边界、协议状态什么时候 commit、失败什么时候可重试、状态如何在线发布、阻塞资源如何与 data plane 隔离。** 这些东西做好以后，再自己写 BufStream / nom parser / `splice(2)`，你会明显更知道自己为什么这么写，而不是为了“zero-copy”这个标签去优化。