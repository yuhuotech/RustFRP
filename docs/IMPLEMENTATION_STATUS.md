# RustFRP V1.0 实现审计与推进清单

审计口径：需求文档 V1.0 + 技术设计 + 当前工作区源码，2026-10-12。
「配置可以解析」不等于「运行时能力已实现」，「构建通过」不等于「三平台实机交付」。本文件作为整个项目的验收清单，逐阶段更新，不能因完成某个增量就标记项目完成。

## 范围与进度

| 模块 / 需求 | 状态 | 现有证据与剩余工作 |
|---|---|---|
| P00 基础工程、CI、Rust/Flutter 覆盖率 | 部分完成 | workspace、CI、基线与真实冒烟已建立；还缺发布、依赖安全检查、性能回归与三平台验收流水线 |
| M-01/11/12 登录、审计、角色 | 已实现待整体验收 | Cookie/CSRF/同源/限流/事务审计已有；管理员可由 Owner 经 REST/面板创建、改角色、启停、重置密码（需理由、事务审计、会话即时吊销，至少保留一位启用的 Owner，不能改自己的角色/状态）；CLI 仍可用 |
| M-02 概览 | 已实现待整体验收 | 在线数/隧道数/版本、当前连接、近 24 小时流量（分钟桶聚合）、Controller 运行时长/常驻内存/累计 CPU 时间（读 `/proc/self`，仅 Linux 有值，其他平台显示「—」）已接入 REST 与面板；需在 Linux 实机复核数值 |
| M-03/SV-03 多 Edge 管理 | 已实现待整体验收 | 独立节点（`rustfrp-agent --mode edge --controller …`）：令牌接入、挑战签名登录、设备名册缓存与持久化、增量/全量下发、设备经节点首次接入、状态与流量回报、断线重连对账；Controller 有节点 CRUD/停用/删除保护、设备令牌指定节点、面板节点列表/详情/接入向导/设备接入选节点、隧道地址按节点显示。真实全栈测试覆盖 Controller 被杀后隧道继续转发、设备重连、Controller 重启后对账（AT-05）。见 `docs/NODES.md`。尚缺：设备跨节点迁移、广域网实测 |
| M-04 客户端管理 | 已实现待整体验收 | 状态、重命名、停用、下线、删除、标签、远程重启 agent、最近登录 IP 均已实现；远程重启仍要求 systemd/launchd 等进程托管，整体验收待补 |
| M-05 接入向导 / G4 | 部分完成 | 客户端与节点接入、脚本托管、正式证书 CA/指纹传递已有；Release 工作流产出多平台 agent 与 SHA256 清单，但下载向导的可信公钥固定、安装器校验闭环和新机器真实安装仍待补 |
| M-06 隧道 CRUD 与热下发 | 部分完成 | TCP/UDP/HTTP/HTTPS/STCP/SUDP、HTTP 完整选项、健康检查、分组、限速、PROXY protocol、受管插件已接入；TCPMUX 与通用加密/压缩仍缺运行时实现，筛选及整体验收待补 |
| M-07 模板、批量启停 | 已实现待整体验收 | SSH/RDP/Web/MySQL/PostgreSQL 模板填充、100 条有界选择、批量事务/逐目标审计/失败回滚/去重下发已实现；Rust/Flutter 行为测试通过 |
| M-08 域名 / 证书 | 部分完成 | 自定义域名、通配、子域名、Host 改写已有；独立证书管理和更多 HTTP 选项待补；ACME 属 P2 |
| M-09/CA-05 流量和连接统计 | 已实现待整体验收 | Edge/Agent 双向字节与连接计数、按隧道有界注册表、Controller 5 秒采样、分钟桶、24h/7d 历史 API 与面板图表筛选均已实现；桌面本机可读取累计统计和已应用隧道摘要；`RUSTFRP_TRAFFIC_RETENTION_DAYS` 配置保留天数，0 表示永久保留。Linux/高并发容量与广域网最终验收待补 |
| M-10/D-10 日志、诊断包 | 部分完成 | Controller 与本机 Agent 均有有界脱敏日志环；私有本机控制通道及 Admin REST 支持游标尾随与缺口提示，Flutter 可轮询新增日志；本机诊断 JSON 导出已有字段白名单、脱敏、权限与不覆盖保护。Controller 诊断包、实际多平台诊断验收待补 |
| M-13 API Token | 已实现待整体验收 | read/tunnels/admin 范围、当前角色上限、哈希存储、撤销/到期/禁用、事务审计、Owner 面板一次性原文与有界保留清理已实现；真实 Bearer 调用/撤销立即失效 E2E 通过 |
| M-14 界面适配 | 已实现 | 两栏可折叠菜单，常规菜单字重，收起隐藏品牌；浅/深、宽/窄、真实字体截图已验；遵守最新用户要求，不恢复三栏 |
| D-01 三平台安装包 | 部分完成 | Release workflow 已定义 macOS arm64/x86_64 DMG/ZIP、Windows x64 setup/ZIP、Linux x64 deb/tar.gz、面板与内置 Agent；构建、签名和公证依赖外部 CI 凭据且尚未实际跑 Release，安装包签名验证、卸载/升级闭环待补 |
| D-02/03/CA-08 桌面受管本机模式 | 部分完成 | 桌面自动定位并安装随包 Agent、私有回环控制、后台启停与恢复已有，macOS 旧版本路径曾实测；当前变更未完成三平台真实主机验收，Windows/Linux 后台和托盘仍需实机复核 |
| D-04/05 本机隧道列表、快速新增 | 部分完成 | 已有受管与独立配置入口、配置校验/热重载、已应用隧道列表、连接日志/诊断/流量摘要；完整本机配置编辑、公网地址复制、按隧道流量仍缺 |
| D-06 管理员控制台 | 已实现 | 与 Web 共用界面；会话恢复/凭据持久策略还需安全复核 |
| D-07 独立本机模式 | 部分完成 | 桌面可选择本地 frpc TOML、启动/停止、查看状态/日志/隧道并热重载；图形化逐项配置编辑仍缺 |
| D-08/CA-06 系统服务、托盘、开机启动、通知 | 部分完成 | Rust 服务命令、Linux systemd/macOS launchd/Windows 任务计划定义，桌面托盘、关闭到托盘、开机启动开关、连接状态通知均已实现并有自动化测试；`*2*` 插件用 `tray_manager`，三平台窗口/托盘/自启动/通知真实主机验收仍待做；Windows Agent 崩溃自动重启未实现 |
| D-09/CA-07 更新 | 未实现 | 下载版本通道、固定可信公钥、签名验证、原子替换、回滚及 App/Agent 协调升级未实现；Release 生成签名清单不等于客户端更新器 |
| CA-01/02/03 接入、重连、热更新 | 部分完成 | 已有实际 TLS 隧道与身份持久化；待复核退避抖动、离线缓存及协议抗重放 |
| SV-01/02 单二进制内置 Edge | 已实现待整体验收 | 单进程 SQLite/axum/Edge；`RUSTFRP_BUILTIN_EDGE=off` 可关闭内置 Edge 只做管理面（必须配置 `RUSTFRP_NODE_ADDR`，否则拒绝启动），面板隐藏内置节点并要求设备令牌指定外置节点 |
| SV-04/05 端口/域名、设备鉴权 | 部分完成 | 范围与冲突、设备停用删除、保留端口（`RUSTFRP_RESERVED_PORTS`，Edge/管理/vhost 监听端口自动保留）、单客户端隧道数上限（`RUSTFRP_MAX_TUNNELS_PER_CLIENT`，创建与配置导入同事务回滚）已有；连接速率、外置 Edge 鉴权缓存待补 |
| SV-06 TLS | 部分完成 | 数据面 TLS1.3、正式 PEM、自签名持久化、受管指纹固定及 CA/指纹校验已有；管理 HTTP 非回环监听强制要求安全 Cookie 与 HTTPS 公网地址，Caddy/nginx/systemd/Compose 部署模板已加；真实生产域名、证书和反向代理部署仍待验收 |
| SV-07 限速 | 已实现 | `bandwidthLimit` / `bandwidthLimitMode`：令牌桶，每代理双向与全部连接共享；client / server 两种位置；tcp / http / https，udp 不支持（兼容报告提示）；Controller 存储/下发/导入导出/面板 KB/s 字段；端到端测试验证限速 |
| SV-08 健康检查、负载均衡 | 已实现 | `healthCheck`（tcp / http 2xx，timeout / maxFailed / interval / path / httpHeaders）由客户端探测，状态变化经 `ClientMsg::Health` 通知 Edge；`loadBalancer.group/groupKey`：tcp 同组共用一个公网端口，http / https 同组同路径共用域名，在健康成员间轮转；组密钥 / 端口不一致被拒绝；受管模式下 Controller 存储 / 校验冲突 / 下发 / 导入导出 / 面板表单，不健康时隧道状态显示为异常 |
| SV-09 Prometheus | 已实现待整体验收 | `RUSTFRP_METRICS_TOKEN`（≥16 字符）开启 `/metrics`，Bearer 认证（定时无关比较），默认不提供；指标：客户端/隧道/节点数、内置节点实时连接与字节、24h 流量、进程内存/CPU/运行时长 |
| SV-10 备份恢复 | 已实现当前要求 | `backup <new-file>` 只读在线快照、完整性/旧版本/权限/不覆盖验证及停服恢复操作流程，见 BACKUP_AND_RETENTION.md；不提供覆盖运行中数据库的自动 restore |
| SV-11 数据保留 | 已实现待整体验收 | 审计与流量分钟桶/采样游标保留期可分别配置（默认 90 天/7 天，0 为永久保留，范围 0..3650）；每轮每表有界分批清理、事务审计/回滚及错误处理已实现 |
| TCP / UDP / HTTPS SNI | 已实现 | 独立与受管实际链路已测；进一步容量、性能和平台验收仍需完成 |
| HTTP | 已实现待整体验收 | 逐请求（保持连接上每个请求）按域名/通配/子域 + 路径前缀（locations）+ routeByHTTPUser 选路，Basic Auth（Edge 校验，密码永不经 REST 返回）、Host 改写、requestHeaders.set、responseHeaders.set（逐响应改写，禁止改 Content-Length / Transfer-Encoding / Connection / Upgrade）、Upgrade 透传、请求走私防护、流水线响应保序；独立模式与受管模式（Controller 存储/下发/热更新/导入导出/面板表单）均已支持，真实全栈测试通过；PROXY protocol、限速、responseHeaders、健康检查与负载均衡组已实现并在兼容报告中列出 |
| STCP / SUDP / visitors | 已实现 | 提供方注册 stcp / sudp 代理（不占公网端口，`secretKey` + `allowUsers`）；访问者开新流向 Edge 出示 `auth_key(sk, ts)` 签名，Edge 校验密钥与用户后把流接到提供方会话，之后是透明通道（sudp 复用 UDP 帧）；独立模式用 `[[visitors]]`，受管模式在面板「客户端详情 → 访问者」创建（目标隧道必须与访问者在同一节点，密钥由 Controller 生成并代为下发）；端到端测试覆盖独立、受管（全栈）两种路径。xtcp（P2P 打洞）与 TCPMUX 未实现并在兼容报告中列出 |
| 本地插件 | 独立与受管模式已实现 | `socks5`（可选 RFC 1929 认证）、`http_proxy`、`static_file`、`unix_domain_socket`、`http2https` / `https2http` / `https2https`、`tls2raw`；Controller 支持 TCP/HTTP/HTTPS 隧道配置，保存结构化选项，私密字段不经 REST 返回，证书路径由 Agent 本机读取且限制 16 KiB；面板可编辑插件 JSON。`virtual_net`、`http2http` 等未实现类型在兼容报告列为未激活；`*2*` 仅 HTTP/1.1，HTTP/2 请求会提示未启用 |
| 通用加密/压缩 | 未实现（PROXY protocol 已实现） | `transport.useEncryption` / `useCompression` 逐代理进入兼容报告并提示；需对照上游实现或明确不支持，不能静默忽略 |
| G8 配置兼容 / 导入导出 | 部分完成 | TOML/YAML/JSON、includes、环境模板、默认/校验/导出和 INI convert 已有；独立模式 reload/status/stop 已实现；受管导入向导现可预览差异、阅读警告、勾选确认并填写理由后事务应用，导出有复制界面。frps dashboard、`webServer.tls`、`assetsDir`、tcpmux、加密/压缩等运行能力仍缺或只进入兼容报告，逐字段继续核对 |
| 管理面离线不影响数据面 | 已实现待整体验收 | 内置节点与 Controller 同进程，杀 Controller 仍会中断内置节点的隧道；使用独立节点时 AT-05 通过（同机回环全栈测试）。需要在真实多机环境复验 |
| G7 官网 / 部署 / P07 发布 | 部分完成 | 静态中文官网、Caddy/nginx/Compose/systemd 模板、跨平台 Release 构建与 Ed25519 清单签名工作流已加入；网站/Release 未部署或发布，Apple 与 release 私钥未配置，安装器/更新器未固定公钥，action 供应链加固和正式安全审计报告待补 |
| 内存 / 吞吐 / 延迟硬验收 | 部分完成 | 2026-10-12 macOS Release 基准：100 条空闲连接 Edge/Client 6.78/7.12 MiB；20 次 512 KiB 回显后 7.12/7.69 MiB，关闭后 7.14/7.70 MiB（上限 10 MiB）。Linux Release、1000 连接、Controller 50 设备、吞吐≥直连90%、新增连接<5ms等仍需实测，不能用目标值充成绩 |

## V1.0 之外（按原需求，不静默改范围）

XTCP / P2P 打洞、KCP/QUIC、frp 线协议互通、移动端、多租户计费、ACME、Webhook 告警列为原文 P2 / V1.1 / V2；仍需保留兼容报告与明确边界。当前 V1.0 仍未全部完成：TCPMUX、传输加密/压缩、桌面更新器和实际多平台/发布验收仍是缺口。

## 推进顺序与门槛

1. 补完 V1.0 P1 运行能力：TCPMUX、按客户端/隧道连接数限制、传输加密/压缩；逐字段对照固定上游版本并加入兼容 fixtures。
2. 完成 CA-07 更新器和桌面完整本地配置编辑：固定可信公钥、校验签名、原子替换、失败回滚与 Agent/App 协调更新。
3. 在 macOS ARM64/Intel、Windows x64、Linux x64 主机验收托盘、自启动、通知、后台 Agent、安装/卸载；配置 CI 凭据后跑签名/公证 Release。
4. 将官网部署至正式域名，使用 Caddy/nginx 反向代理完成 HTTPS、Cookie、WebSocket 验收；完成安全审计报告与依赖供应链加固。
5. 做设备跨节点迁移与 Linux/高并发性能复测，再评估 V1.1 的 XTCP、KCP/QUIC、frp 线协议互通。

每步：协议先行，带断言的失败测试先行，fmt/clippy/analyze 全绿；每 crate 行/函数/分支 ≥90% 且不低于既有基线；Flutter 不低于基线；运行时改动复测内存。不可用降低基线或未实现的空壳来凑完成率。

外部交付条件：macOS 账号已有但 CI 证书/公证凭据未接入；Windows/Linux 的实机、官网部署与 Release 托管需在对应环境验证。先完成代码和可复核产物，仅在确需外部凭据/环境时提出具体事项。

Hub 有界性修复：受管会话与待处理下线事件共用 256 个许可，下线事件消费完成才归还许可；超限登录明确拒绝，队列不丢下线事件。最后一个会话下线后移除计数和配置通道；下线清理与接入串行，防止旧清理删除重连的新通道。增量为固定容量队列/信号量，未引入线程；macOS 常见场景内存基准通过，Linux、高并发吞吐及 1000 连接场景仍需验收。

## 2026-10-12 最终联调摘要

`make lint`、`make test`、`make cov`、Flutter analyze/132 项测试、`make app-cov`、`make web-build`、`make e2e`、`make bench-mem` 均通过。Rust 覆盖率见 `docs/HANDOFF.md`；Agent 行覆盖率比基线低 0.04 个百分点，处于 `check_lcov.py` 实际执行的 0.05 容差内。macOS ARM64 Release `.app` 可构建（51.5 MB），本轮未在 Windows/Linux 主机安装验收，未发布带签名 Release；这些限制不表示 V1.0 已完成。
