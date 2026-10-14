# 本机控制协议 v1

受管与独立 client 启用 `--local-control <file>` 时存在；不改变 frp 配置或隧道协议。
Agent 在 `127.0.0.1` 的随机端口监听，发现文件为当前用户私有 JSON：
`{"version":1,"port":12345,"token":"64位随机十六进制","controller":"host:port"}`。
发现文件不包含接入令牌或身份私钥。旧发现文件的端口用作互斥：仍被占用时不启动第二个 agent；正常停止后保留发现文件，下次启动重新生成令牌。
受管身份为原始 Edge 地址；独立身份为 `standalone:<完整配置文件绝对路径>`。桌面必须核对该身份，不能将其他状态目录中的发现文件当成本机当前配置。

一个 TCP 连接执行一个换行结尾的 JSON 请求，最大 1024 字节，读写超时 2 秒。服务端串行处理，最多一个请求缓冲，无 per-connection 线程。请求 `{"token":"…","command":"status"}` 或 `stop`。
响应参见 fixtures；错误返回 `{"error":"unauthorized"}` / `invalid_request`。`stop` 返回 `{"ok":true}` 后取消 agent。错误认证不会停止或暴露状态。只允许回环连接；不接受浏览器 HTTP 请求。

状态里的 `connected` 来自隧道连接事件，`proxies` 与 `revision` 来自已应用的配置事件；断连会清除 connected 和 proxies。运行不代表隧道已在线。
桌面退出不发送 stop；仅显式「停止」才结束后台 agent。应用重开从发现文件重新连接。原生桌面以 macOS GUI 域 launchd 作业或独立监督进程运行 Agent：异常退出有有界退避，更新/重启退出码 75 可恢复；正常退出码 0 结束监督。后台作业与登录自启动是分别配置的能力，不能由「Agent 仍在运行」推断已启用开机启动。
首次受管接入仅在临时进程中使用一次性令牌；确认身份已保存后通过认证控制停止该进程，转为不含令牌的后台作业。令牌不能写入偏好、后台定义或日志。已运行时启动应幂等；仅发现文件不存在或明确拒绝连接可判定停止，超时、认证失败或身份不匹配不能触发第二个 Agent。

`status` 现增加可选 `traffic` 对象：`bytes_in` 为本机成功收到的代理负载，`bytes_out` 为成功返回 Edge 的负载，`connections` 为当前转发会话（UDP 为关联），`connections_total` 为本次运行累计建立次数。TCP/HTTP/HTTPS 含实际转发首部、TLS 透传数据，UDP 不含封装头。字段即时读取固定原子计数器；这是本机统计，不再次上传到 Controller，避免重复累计。旧版本没有此对象时桌面提示更新 agent，不显示伪造的零流量。进程重启累计归零。

`logs` 命令沿用发现令牌认证，返回最多 128 条、每条最多 512 个 UTF-8 字节的运行日志。请求可带非负整数 `after`，响应为 `items`、`cursor`、`gap`、`capacity` 与 `line_bytes`；省略 `after` 保留完整快照行为。`gap` 表示旧日志被覆盖或 Agent 重启使游标失效；失效时回放当前有界尾部。环形缓冲按条数和单条长度双重限额，日志不含流量载荷；含 token/password/secret/authorization/private key 的消息整条隐藏。旧 Agent 没有游标时新桌面使用有界快照，并提示升级，不人为编造游标。

`tunnels` 命令返回 Agent 当前已应用配置中的本地目标摘要：`revision`、`items[{id,kind,local_ip,local_port,remote_port}]`。摘要最多 1024 条，每个文本最多 256 字节；控制请求可带 `offset`，每页最多 32 条，并返回 `next_offset`（没有下一页时为 null）。桌面在同一配置版本内有界读取分页，版本变化时要求刷新。不包含 Host 改写或秘密字段。列表代表已应用配置，不代表 Edge 监听成功；远程端口 0 表示尚未查询到自动分配地址，不能伪装为可用公网地址。断线时清空当前列表。原生本机页面已接入状态、隧道、日志与诊断导出，完整桌面验收进度见 `docs/UI_REFACTOR_STATUS.md`。

受管接入支持 `--tls-pin <64位SHA256十六进制>`：固定服务端叶证书 DER 的 SHA-256，握手签名仍验证；若同时配置 CA，则 CA/域名校验与指纹必须同时通过。来源为可信管理面生成的接入命令；不能通过未校验连接自动接受新指纹。证书轮换需要更新固定值，原值不能静默回退。
