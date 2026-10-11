# 本机控制协议 v1

仅受管 client 启用 `--local-control <file>` 时存在；不改变 frp 配置或隧道协议。
Agent 在 `127.0.0.1` 的随机端口监听，发现文件为当前用户私有 JSON：
`{"version":1,"port":12345,"token":"64位随机十六进制","controller":"host:port"}`。
发现文件不包含接入令牌或身份私钥。旧发现文件的端口用作互斥：仍被占用时不启动第二个 agent；正常停止后保留发现文件，下次启动重新生成令牌。

一个 TCP 连接执行一个换行结尾的 JSON 请求，最大 1024 字节，读写超时 2 秒。服务端串行处理，最多一个请求缓冲，无 per-connection 线程。请求 `{"token":"…","command":"status"}` 或 `stop`。
响应参见 fixtures；错误返回 `{"error":"unauthorized"}` / `invalid_request`。`stop` 返回 `{"ok":true}` 后取消 agent。错误认证不会停止或暴露状态。只允许回环连接；不接受浏览器 HTTP 请求。

状态里的 `connected` 来自隧道连接事件，`proxies` 与 `revision` 来自已应用的配置事件；断连会清除 connected 和 proxies。运行不代表隧道已在线。
桌面退出不发送 stop；仅显式「停止」才结束后台 agent。应用重开从发现文件重新连接。此阶段后台进程不等同于系统服务，尚不提供操作系统重启后的自动启动。

`status` 现增加可选 `traffic` 对象：`bytes_in` 为本机成功收到的代理负载，`bytes_out` 为成功返回 Edge 的负载，`connections` 为当前转发会话（UDP 为关联），`connections_total` 为本次运行累计建立次数。TCP/HTTP/HTTPS 含实际转发首部、TLS 透传数据，UDP 不含封装头。字段即时读取固定原子计数器；这是本机统计，不再次上传到 Controller，避免重复累计。旧版本没有此对象时桌面提示更新 agent，不显示伪造的零流量。进程重启累计归零。

已增加 `logs` 命令：沿用发现令牌认证，返回最多 128 条、每条最多 512 个 UTF-8 字节的运行日志快照。环形缓冲按条数和单条长度双重限额，日志不含流量载荷；含 token/password/secret/authorization 的消息整条隐藏。命令已接入受管本机 Agent，Controller 日志、桌面查看入口和诊断包尚未接入。

已实现 Agent 与桌面读取层，界面待接入：`tunnels` 命令返回 Agent 当前已应用配置中的本地目标摘要：`revision`、`items[{id,kind,local_ip,local_port,remote_port}]`。摘要最多 1024 条，每个文本最多 256 字节；控制请求可带 `offset`，每页最多 32 条，并返回 `next_offset`（没有下一页时为 null）。不包含 Host 改写或秘密字段。列表代表已应用配置，不代表 Edge 监听成功；远程端口 0 表示尚未查询到自动分配地址，不能伪装为可用公网地址。断线时清空当前列表。

受管接入将支持 `--tls-pin <64位小写SHA256>`：固定服务端叶证书 DER 的 SHA-256，握手签名仍验证；若同时配置 CA，则 CA/域名校验与指纹必须同时通过。来源为可信管理面生成的接入命令；不能通过未校验连接自动接受新指纹。证书轮换需要更新固定值，原值不能静默回退。该能力正在实现。
