# 024: Web 抓取层使用请求库，地址策略在请求之前执行

Decision status: Proposed.

## Context

抓取层原来自己建连：解析主机名、拼 `CONNECT`、建 TLS、按地址逐个重试，约 970 行实现。
它存在的理由是 A.4 要求“校验每个解析结果再决定怎么连”。地址策略改成可选
（[#1607](https://github.com/arcships/dim/issues/1607)）之后，这条理由只剩下开关打开的那一半，
而模块的体量与它提供的两个工具、七个参数不匹配（[#1609](https://github.com/arcships/dim/issues/1609)）。

## Decision

### D1. 传输使用 `reqwest`

仅 HTTP/1，关闭重定向，10 秒连接时限，手工 gzip/deflate 解码与 5 MiB 上限，`User-Agent`
保持 `dim-web/<版本>`。重定向仍由 Web 自己按主机组处理，不交给客户端。

### D2. `blockPrivateAddresses` 在请求之前运行

开启时，Web 在建立请求之前解析一次主机名、按封禁范围检查每个结果；没有代理时还会把第一个
允许的地址固定给本次请求。关闭时不解析。它做且只做这件事：它是请求前的检查，不是连接层的保证。

### D3. 代理的目标形式按 scheme 区分

`https` 目标用 `CONNECT <主机>:<端口>`，`http` 目标以 absolute-form 交给代理，代理因此能看到
完整 URL。代理来源顺序不变：配置的 `proxy`、环境变量、桌面设置，`DIM_WEB_SYSTEM_PROXY=off`
关闭后两级。

### D4. 网络错误只有 `dns_failed` 与 `connect_failed`

TLS 失败并入 `connect_failed`；代理拒绝隧道是 `connect_failed`；代理直接作答时按其状态码
报 `http_status`。

### D5. `proxyDns` 决定谁解析主机名

代理收到的始终是主机名：`https` 进入 `CONNECT` 行，`http` 进入 absolute-form 请求行。
`true`（默认）时这就是唯一的解析；`false` 时 Web 先解析并检查，再发出同一个主机名，因此有代理
时代理解析，`proxyDns: false` 只增加一次请求前的检查。没有任何代理时该开关不生效。

## Consequences

- 自建 TCP/TLS、`CONNECT` 解析与逐地址重试全部删除，A.4 的描述随之改为上面的行为。
- 代理夹具同时支持隧道与明文转发，契约测试按 scheme 分开断言。
- Appendix C 少一个错误码；A15 的假设改为“代理接受两种目标形式”。

References: [Web 设计](../design/plugins/Web.md), [#1609](https://github.com/arcships/dim/issues/1609)。
