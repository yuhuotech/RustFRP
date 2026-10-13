# RustFRP V1.0 实现审计与推进清单

审计口径：需求文档 V1.0 + 技术设计 + 当前工作区源码，2026-10-13。
「配置可以解析」不等于「运行时能力已实现」，「构建通过」不等于「三平台实机交付」。本文件作为整个项目的验收清单，逐阶段更新，不能因完成某个增量就标记项目完成。

本轮用户指定双主机 Linux / Web 联调、修复、网络范围和剩余条件见 [2026-10-13 验收记录](WAN_ACCEPTANCE_20261013.md)。下方 2026-10-12 质量表为历史基线，最新数字和日志按该记录验收。

## 范围与进度

| 模块 / 需求 | 状态 | 现有证据与剩余工作 |
|---|---|---|
| P00 基础工程、CI、Rust/Flutter 覆盖率 | 已完成 | workspace、CI、覆盖率校验与真实冒烟已建立；Rust crates 的行/函数/分支覆盖率及 Flutter 行覆盖率均 ≥90%，`make cov` 通过。90% 是统一最低门槛，不再执行高于 90% 的历史回归门槛 |
| M-01/11/12 登录、审计、角色 | 已实现待整体验收 | Cookie/CSRF/同源/限流/事务审计已有；管理员可由 Owner 经 REST/面板创建、改角色、启停、重置密码（需理由、事务审计、会话即时吊销，至少保留一位启用的 Owner，不能改自己的角色/状态）；CLI 仍可用 |
| M-02 概览 | 已实现待整体验收 | 在线数/隧道数/版本、当前连接、近 24 小时流量（分钟桶聚合）、Controller 运行时长/常驻内存/累计 CPU 时间（读 `/proc/self`，仅 Linux 有值，其他平台显示「—」）已接入 REST 与面板；需在 Linux 实机复核数值 |
| M-03/SV-03 多 Edge 管理 | Linux 双主机验收通过 | 令牌接入、挑战签名登录、名册缓存、增量/全量下发、流量/状态回报；内置/外置双向在线迁移、两台物理主机迁移与 AT-05 已通过，NAT 侧节点经现有 Tailscale WAN 私网访问；目标 TLS 指纹和地址持久化。Visitor 跨节点限制仍明确拒绝。见 `docs/NODES.md` 和本轮验收记录 |
| M-04 客户端管理 | 已实现代码，目标主机验收待做 | 状态、重命名、停用、下线、删除、标签、连接上限、Agent 更新命令、最近登录 IP 均已实现；远程重启/升级退出码由桌面或 systemd/launchd 等进程监督器协调，真实服务管理器验收仍待做 |
| M-05 接入向导 / G4 | 已实现代码，真实签名发布待做 | 客户端与节点接入、脚本托管、正式证书 CA/指纹传递、Agent 签名更新公钥固定与摘要校验闭环已实现；Release 工作流未配置真实签名所需密钥/公钥变量，未发布真实签名包 |
| M-06 隧道 CRUD 与热下发 | 已实现（含明确协议边界） | TCP/UDP/HTTP/HTTPS/TCPMUX(httpconnect)/STCP/SUDP、HTTP 选项、TCP 流与 Visitor 工作流加密/压缩、客户端/隧道/UDP peer/SUDP visitor workflow 上限、健康检查、分组、限速、PROXY protocol、受管插件均已接入；TCPMUX passthrough、Visitor 加密/压缩、UDP/SUDP 上限均有实际链路测试 |
| M-07 模板、批量启停 | 已实现待整体验收 | SSH/RDP/Web/MySQL/PostgreSQL 模板填充、100 条有界选择、批量事务/逐目标审计/失败回滚/去重下发已实现；Rust/Flutter 行为测试通过 |
| M-08 域名 / 证书 | 部分完成 | 自定义域名、通配、子域名、Host 改写已有；独立证书管理和更多 HTTP 选项待补；ACME 属 P2 |
| M-09/CA-05 流量和连接统计 | 已实现待整体验收 | Edge/Agent 双向字节与连接计数、按隧道有界注册表、Controller 5 秒采样、分钟桶、24h/7d 历史 API 与面板图表筛选均已实现；桌面本机可读取累计统计和已应用隧道摘要；`RUSTFRP_TRAFFIC_RETENTION_DAYS` 配置保留天数，0 表示永久保留。Linux/高并发容量与广域网最终验收待补 |
| M-10/D-10 日志、诊断包 | 已实现代码，目标主机验收待做 | Controller 与本机 Agent 均有有界脱敏日志环；私有本机控制通道及 Admin REST 支持游标尾随与缺口提示，Flutter 可轮询新增日志；桌面诊断 JSON 导出使用字段白名单、脱敏、私有权限和不覆盖保护；诊断内容不含令牌、密钥与流量正文 |
| M-13 API Token | 已实现待整体验收 | read/tunnels/admin 范围、当前角色上限、哈希存储、撤销/到期/禁用、事务审计、Owner 面板一次性原文与有界保留清理已实现；真实 Bearer 调用/撤销立即失效 E2E 通过 |
| M-14 界面适配 | 已实现 | 两栏可折叠菜单，常规菜单字重，收起隐藏品牌；浅/深、宽/窄、真实字体截图已验；遵守最新用户要求，不恢复三栏 |
| D-01 三平台安装包 | 实现代码，发布/主机验收待做 | Release workflow 已定义 macOS arm64/x86_64 DMG/ZIP、Windows x64 setup/ZIP、Linux x64 deb/tar.gz、面板与内置 Agent；真实签名 Release、公证与目标平台安装验收属于本轮明确保留项 |
| D-02/03/CA-08 桌面受管本机模式 | 已实现代码，目标主机验收待做 | 桌面自动定位并安装随包 Agent、私有回环控制、后台启停与恢复、托盘、关闭到托盘、开机启动和连接通知均有实现与自动化测试；三平台真实主机安装、后台进程和系统服务验收属于保留项 |
| D-04/05 本机隧道与配置编辑 | 已实现（文本配置编辑形态） | 受管/独立入口、已应用隧道列表、连接日志/诊断/流量摘要已有；独立模式可完整编辑 TOML/YAML/JSON 文件，保存前调用 Agent 校验并暂存/原子替换。当前交付的是完整配置文件编辑器，不是逐字段表单 |
| D-06 管理员控制台 | 已实现 | 与 Web 共用界面；会话恢复/凭据持久策略还需安全复核 |
| D-07 独立本机模式 | 已实现（完整配置文件文本编辑） | 桌面可选择本地配置文件、启动/停止、查看状态/日志/隧道、热重载；完整 TOML/YAML/JSON 文件可编辑，保存前调用 Agent 校验并原子替换。图形化逐字段表单不是当前本地文件编辑器的交付形态 |
| D-08/CA-06 系统服务、托盘、开机启动、通知 | 部分完成 | Rust 服务命令、Linux systemd/macOS launchd/Windows 任务计划定义，桌面托盘、关闭到托盘、开机启动开关、连接状态通知均已实现并有自动化测试；`*2*` 插件用 `tray_manager`，三平台窗口/托盘/自启动/通知真实主机验收仍待做；Windows Agent 崩溃自动重启未实现 |
| CA-07 Agent 更新 | 已实现代码，真实发布待配置 | Controller 命令携带版本；Agent 下载 Ed25519 签名 SHA256 清单及 Release 版本标记、验签和流式校验二进制、同目录替换并失败回滚，更新后退出并由桌面/系统服务协调重启；离线签名/摘要/替换测试通过。真实签名密钥与 `RUSTFRP_RELEASE_PUBLIC_KEY_HEX` 尚未配置 |
| D-09 Flutter App 更新 | 已实现代码，真实签名包待配置 | 桌面 App 更新器支持 GitHub Release 与国内加速源、签名清单/版本标记校验、包摘要验证、下载进度/取消和 macOS/Windows/Linux 安装策略；缺可信 Release 公钥时 fail closed。真实签名 Release 尚未发布 |
| CA-01/02/03 接入、重连、热更新 | 部分完成 | 已有实际 TLS 隧道与身份持久化；待复核退避抖动、离线缓存及协议抗重放 |
| SV-01/02 单二进制内置 Edge | 已实现待整体验收 | 单进程 SQLite/axum/Edge；`RUSTFRP_BUILTIN_EDGE=off` 可关闭内置 Edge 只做管理面（必须配置 `RUSTFRP_NODE_ADDR`，否则拒绝启动），面板隐藏内置节点并要求设备令牌指定外置节点 |
| SV-04/05 端口/域名、设备鉴权 | 已实现待整体验收 | 范围与冲突、设备停用/删除、单隧道数上限、按客户端/隧道 TCP 并发连接上限、保留端口（`RUSTFRP_RESERVED_PORTS`）、远程 Agent 更新及节点迁移已有；连接速率、外置 Edge 鉴权缓存和广域网验收待补 |
| SV-06 TLS / Dashboard | Controller 公网与独立 TLS/CA 已验 | 真实 HTTPS 域名、证书、nginx 反代与 Secure Cookie，受管固定指纹和独立 trustedCaFile 已通过。frps 独立 dashboard 的只读 API / Basic Auth 已实测，独立 `webServer.tls` / `assetsDir` 公网组合仍保留专项验收 |
| SV-07 限速 | 已实现 | `bandwidthLimit` / `bandwidthLimitMode`：令牌桶，每代理双向与全部连接共享；client / server 两种位置；tcp / http / https，udp 不支持（兼容报告提示）；Controller 存储/下发/导入导出/面板 KB/s 字段；端到端测试验证限速 |
| SV-08 健康检查、负载均衡 | 已实现 | `healthCheck`（tcp / http 2xx，timeout / maxFailed / interval / path / httpHeaders）由客户端探测，状态变化经 `ClientMsg::Health` 通知 Edge；`loadBalancer.group/groupKey`：tcp 同组共用一个公网端口，http / https 同组同路径共用域名，在健康成员间轮转；组密钥 / 端口不一致被拒绝；受管模式下 Controller 存储 / 校验冲突 / 下发 / 导入导出 / 面板表单，不健康时隧道状态显示为异常 |
| SV-09 Prometheus | 已实现待整体验收 | `RUSTFRP_METRICS_TOKEN`（≥16 字符）开启 `/metrics`，Bearer 认证（定时无关比较），默认不提供；指标：客户端/隧道/节点数、内置节点实时连接与字节、24h 流量、进程内存/CPU/运行时长 |
| SV-10 备份恢复 | 已实现当前要求 | `backup <new-file>` 只读在线快照、完整性/旧版本/权限/不覆盖验证及停服恢复操作流程，见 BACKUP_AND_RETENTION.md；不提供覆盖运行中数据库的自动 restore |
| SV-11 数据保留 | 已实现待整体验收 | 审计与流量分钟桶/采样游标保留期可分别配置（默认 90 天/7 天，0 为永久保留，范围 0..3650）；每轮每表有界分批清理、事务审计/回滚及错误处理已实现 |
| TCP / UDP / HTTPS SNI | 已实现 | 独立与受管实际链路已测；进一步容量、性能和平台验收仍需完成 |
| HTTP | 已实现待整体验收 | 逐请求（保持连接上每个请求）按域名/通配/子域 + 路径前缀（locations）+ routeByHTTPUser 选路，Basic Auth（Edge 校验，密码永不经 REST 返回）、Host 改写、requestHeaders.set、responseHeaders.set（逐响应改写，禁止改 Content-Length / Transfer-Encoding / Connection / Upgrade）、Upgrade 透传、请求走私防护、流水线响应保序；独立模式与受管模式（Controller 存储/下发/热更新/导入导出/面板表单）均已支持，真实全栈测试通过；PROXY protocol、限速、responseHeaders、健康检查与负载均衡组已实现并在兼容报告中列出 |
| STCP / SUDP / visitors | 已实现（XTCP 属后续范围） | 提供方注册 stcp / sudp 代理，访问者凭签名鉴权后接入；独立与受管路径均有测试。Visitor 的 `transport.useEncryption/useCompression` 已持久化、下发并在工作流两端应用；XTCP 属 P2/V1.1 |
| 本地插件 | 独立与受管模式已实现 | `socks5`（可选 RFC 1929 认证）、`http_proxy`、`static_file`、`unix_domain_socket`、`http2https` / `https2http` / `https2https`、`tls2raw`；Controller 支持 TCP/HTTP/HTTPS 隧道配置，保存结构化选项，私密字段不经 REST 返回，证书路径由 Agent 本机读取且限制 16 KiB；面板可编辑插件 JSON。`virtual_net`、`http2http` 等未实现类型在兼容报告列为未激活；`*2*` 仅 HTTP/1.1，HTTP/2 请求会提示未启用 |
| 通用加密/压缩 | 已实现（TCP 工作流） | TCP 代理与启用选项的 STCP/SUDP Visitor 工作流使用每流随机密钥、AES-256-GCM、独立方向密钥/序号和有界 LZ4 记录；公网 UDP 代理不应用流加密/压缩 |
| 连接数限制 | 已实现 | `maxConnectionsPerClient` 与隧道 `maxConnections` 共享原子许可；UDP 上限按活跃源地址计数，受全局 peer 上限约束；SUDP 上限按访问者工作流计数；REST、存储、配置导入导出和面板均支持 |
| TCPMUX | 已实现（HTTP CONNECT，含 passthrough） | `multiplexer=httpconnect` + Edge `tcpmuxHTTPConnectPort`；按 CONNECT Host 路由并复用单监听端口；`tcpmuxPassthrough` 可选择完整转发 CONNECT 请求；LB/健康检查组合明确拒绝 |
| frps dashboard / `webServer.tls` / `assetsDir` | 已实现代码，公网部署待验收 | 独立只读 dashboard JSON API、Basic Auth、TLS、内置首页与安全静态资源服务已接入；独立客户端管理 WebServer TLS/assets 也已支持；公网部署属于保留项 |
| G8 配置兼容 / 导入导出 | 已实现（按兼容边界提示） | TOML/YAML/JSON、includes、环境模板、默认/校验/导出和 INI convert 已有；独立模式 reload/status/stop 已实现；受管导入可预览差异、确认警告并填写理由后事务应用。尚不支持字段会进入兼容报告，不静默忽略 |
| 管理面离线不影响数据面 | 双主机 AT-05 通过 | 内置节点与 Controller 同进程，停止 Controller 会中断内置节点隧道；独立 Edge 继续转发，客户端重启凭缓存名册签名登录，控制面恢复全量对账。公网主机独立 Edge 和另一台物理主机 Edge 均实测 |
| G7 官网 / 部署 / P07 发布 | 部分完成 | 静态中文官网、Caddy/nginx/Compose/systemd 模板、跨平台 Release 构建与 Ed25519 清单签名工作流已加入；网站/Release 未部署或发布，Apple 凭据与 Release 私钥/公钥变量未配置，Action 供应链加固和正式安全审计报告待补 |
| 内存 / 吞吐 / 延迟硬验收 | Linux 100 连接通过，重载仍待验 | 2026-10-13 每方向转发缓冲 4 KiB：Linux Release 10 隧道，空载 Edge/Client 7.54/7.84 MiB、100 空闲连接 8.56/9.27 MiB、20 次 512 KiB 回显及关闭后 8.94/9.57 MiB。Controller 配置 glibc 阈值后真实面板请求约 11 MiB。1000 连接、Controller 50 设备、吞吐≥直连90%、新增连接<5ms仍待专项实测 |

## 最终联调结果（2026-10-12）

| 验收项 | 结果 |
|---|---|
| `make lint`、`make test` | 通过 |
| `make app-test` | Flutter analyze 通过，158 项测试通过 |
| `make e2e` | 通过；真实 Controller/Agent/REST TCP 300000 字节回显、UDP/HTTP、重连及在线备份检查通过 |
| `make web-build` | 通过，未使用外部 CDN |
| `make bench-mem` | 通过；Edge/Client 在 100 条连接与 20 次回显后均低于 10 MiB |
| Core 覆盖率 | 行/函数/分支 99.03/97.97/97.16%，均超过 90% 最低门槛 |
| Server 覆盖率 | 行/函数/分支 97.16/95.79/92.88%，均超过 90% 最低门槛 |
| Flutter 覆盖率 | 行 97.26%，超过 90% 最低门槛 |
| Agent 覆盖率 | 行/函数/分支 97.08/91.40/91.37%，均超过 90% 最低门槛 |

指定功能均已实现并有自动化测试。2026-10-13 新增 Linux 双主机/HTTPS 面板联调、原 frp 不干扰检查、跨主机迁移和离线恢复验收。V1.0 整体验收仍未完成：成功的真实签名升级、官网发布、多平台 Flutter 桌面实机验收、重载性能和正式安全审计仍未完成；不将安全拒绝更新当作成功升级。

## V1.0 之外（按原需求，不静默改范围）

XTCP / P2P 打洞、KCP/QUIC、frp 线协议互通、移动端、多租户计费、ACME、Webhook 告警列为原文 P2 / V1.1 / V2；继续保留兼容报告与明确边界。

## 推进顺序与门槛

1. 配置真实 Release 签名私钥和匹配公钥，发布并验证签名 Release。
2. 发布官网并完成安全审计；Controller 公网 HTTPS/Cookie 已通过，配置下发走隧道控制流，无 WebSocket。
3. 在 macOS ARM64/Intel、Windows x64、Linux x64 主机验收 Flutter 桌面安装/更新、托盘、自启动、通知；Linux Agent/systemd 与跨主机迁移已通过本轮环境验证。
4. 复测 Linux/高并发性能；之后再评估 V1.1 的 XTCP、KCP/QUIC 和协议互通。

每步：协议先行、重要行为有有效断言、fmt/clippy/analyze 全绿；每 crate 行/函数/分支 ≥90%；运行时改动复测内存。当前 lint、Rust/Flutter 全量测试、覆盖率、Web 构建、E2E、内存预算均通过。

外部交付条件：真实 Release 签名密钥与多平台 Flutter 桌面完整实机验收仍缺；本轮双主机网络与 HTTPS 面板已配置并验证，不能替代其他平台或正式安全审计。

Hub 有界性修复：受管会话与待处理下线事件共用 256 个许可，下线事件消费完成才归还许可；超限登录明确拒绝，队列不丢下线事件。最后一个会话下线后移除计数和配置通道；下线清理与接入串行，防止旧清理删除重连的新通道。增量为固定容量队列/信号量，未引入线程；macOS 和 Linux 100 连接内存已通过，高并发吞吐及 1000 连接场景仍待专项验收。
