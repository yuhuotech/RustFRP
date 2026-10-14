# AGENTS.md — RustFRP 协作约定

> 给人和 AI 编程助手共同使用。**开始任何工作前先读：** `docs/RUSTFRP_TECH_DESIGN_V1.0.md` §0（两条最高原则）与 `docs/DEVELOPMENT_STANDARDS.md`。

> **涉及 UI 时额外必读：** [`docs/UI_SPEC.md`](docs/UI_SPEC.md)。用户确认的菜单命名、紧凑指标卡、管理表格、详情分组与配色以该文件为准；修改界面约定时同步更新该文件。

## 两条最高原则
1. **极致省内存**：典型场景 Agent / Edge 常驻 ≈10 MB 以内。新增功能须评估增量内存，避免无界容器、整包缓冲、per-connection 线程。
2. **配置层完全兼容 frp**：`frps.toml` / `frpc.toml` 无需修改即可使用；不支持的字段必须进入兼容报告，**不得静默忽略**。frp 官方源码是行为的最终依据（语料见 `rustfrp-protocol/frp-compat/upstream`）。

## 强制门槛
- 每个 Rust crate：**行 / 函数 / 分支覆盖率均 ≥ 90%**，且不得低于 `tools/cov/baseline.json`；新增代码要有有效断言的测试。
- `cargo fmt --check`、`cargo clippy --all-targets -D warnings`；非测试代码禁止 `unwrap/expect/panic/索引`，`unsafe` 默认 forbid。

## 常用命令
```sh
make lint        # fmt --check + clippy
make test        # cargo test --workspace
make cov         # 全部 crate 覆盖率（行/函数/分支）并校验门槛与基线
make bench-mem   # release 内存基准（≤10 MB）
make app-test    # flutter analyze + test
make app-cov     # Flutter 行覆盖率（≥90%）
make web-build   # 面板 Web（必须 --no-web-resources-cdn）
make e2e         # 全栈冒烟：真实 server + agent + REST
make app-shots   # 评审截图（真实字体）
```

Flutter：颜色、字号、圆角沿用 linkory 的 token / 共享控件，action 主色为草绿（`rustfrp-app/lib/theme/tokens.dart`）。**按用户要求，宽屏采用两栏布局：可折叠菜单（最新展开宽度 160px / 收起 72px）+ 内容工作区，不再设置中间列表栏。** 按用户最新的避免拥挤要求，菜单行高 **40px**、主菜单行间距 **6px**、图标与文字间距 **12px**；文字沿用 13px token，详情与列表在内容区内切换。

## UI 协作约定（2026-10-13）

- 菜单/页名统一为 **概览、节点管理、客户端、隧道管理、审计日志、系统设置**；系统设置位于左下角，窄屏导航使用同名。
- 展开侧栏时顶部 Logo 左侧与主菜单图标左侧对齐，距侧栏左边缘均为 20px。
- 菜单选中态使用草绿淡底、1px 草绿半透明边框、草绿文字与 medium 字重，保留分类图标色；收起状态也保留背景与边框，不能改变行高和图标对齐。
- 概览六张指标卡保持紧凑：宽内容区六卡一行，常规字号高约 96px，窄屏自动换行；控制器版本不独占整行。详细尺寸与断点见 `docs/UI_SPEC.md`。
- 管理列表优先采用全宽表格：清晰表头、细行分隔、适度行高（通常 64–80px）、独立操作列，补足状态、版本、所属资源、地址及有效期等真实信息。长文本可查看全文，窄屏滚动或卡片适配不能隐藏操作。
- 详情采用顶部摘要与分组信息卡，宽屏双列、窄屏单列；状态、网络、能力、身份与管理操作有明确层级。优先复用 `shared/workspace_widgets.dart`、`shared/panel_widgets.dart` 与 `shared/widgets.dart`。
- 保持草绿主操作色，协调使用蓝色（连接/地址/身份）、绿色（正常状态/隧道）、橙色（凭据/待处理）、红色（异常/拒绝）；复用深浅色 token，颜色必须搭配文字语义，不做大面积高饱和填充。
- “节点”指服务端 Edge，区分内置与独立服务端节点，不能与客户端混淆；在线数、接入数、局部加载数等统计口径不能混用，也不能编造后端没有提供的监控指标。
- UI 重排必须保留权限、审计理由、刷新/重试、批量操作、配置导入导出、设备迁移、更新和凭据只显示一次等既有能力。验证深浅色/宽窄屏/空错误状态与实际操作，运行 `make app-test`、`make app-cov`、`make web-build` 并检查真实字体截图与浏览器效果。
- 本地预览使用 `./start_dev.sh`，Web 端口固定为 **9674**；不得为查看 UI 自动切换到其他端口。

## 仓库结构（现状）
- `rustfrp-core/`：Rust 共享库。`frpcfg`（frp 配置兼容层：解析 / 默认值 / 校验 / 兼容报告 / 导出 / INI 转换）、`wire` / `mux` / `tls` / `identity`（线协议、yamux、TLS、Ed25519）、`vhost`、`udpframe`、`pipe`。
- `rustfrp-agent/`：同一二进制的 client / edge / 受管模式（含受管 Edge：`node`）；`service`（系统服务）、`convert`、`local_control`（桌面回环控制）、`log_ring`、`metrics`（有界流量计数）。
- `rustfrp-server/`：Controller（`store` / `api` / `hub` / `traffic` / `api_tokens` / `config_transfer` / `maintenance`）。
- `rustfrp-app/`：Flutter（Web 面板 + 桌面）。
- `rustfrp-protocol/`：兼容性语料（`frp-compat`）、fixtures 与各协议说明（API_TOKENS / CONFIG_IMPORT / DIAGNOSTICS / LOCAL_CONTROL / METRICS / TUNNEL_BATCH）。
- `installer/`：`install.sh` / `install.ps1`（由 Controller 托管）。`tools/`：覆盖率、内存基准、e2e、安装器工具。
- `docs/`：需求、技术设计、开发规范；**`docs/IMPLEMENTATION_STATUS.md` 是逐项验收清单，进度以它为准**。
- 待建：`rustfrp-web/`（官网）、`deploy/`。

## 架构要点（Controller）
- `rustfrp-server` = SQLite（单连接 + spawn_blocking）+ axum 管理 API + **内置 Edge**（`rustfrp-agent::edge`，通过 `Policy` 接入 `DbPolicy`）。
- 设备接入：Client 用一次性令牌换身份（Ed25519 公钥入库）；之后用签名登录。**配置下发复用隧道连接**（控制流上的 `ServerMsg::Config` 全量快照，Edge 对账增删代理，无 WebSocket）。
- 代理名即隧道 ID；隧道状态由 Edge 回报（`ProxyEvent`）写回 DB。
- 管理 API：Cookie 会话 + CSRF + 同源检查 + 登录限流；写操作必须带 `reason` 并与业务同事务写审计。首个管理员只能经 CLI（`rustfrp-server admin create`）创建；其后 Owner 可经 API/面板管理管理员与角色。

## 当前进度
> 详细逐项状态见 `docs/IMPLEMENTATION_STATUS.md`；此处只给概览。

- P00：Cargo workspace、CI、覆盖率门槛工具、Flutter 工程 ✔。
- P01 隧道核心 ✔：`frpcfg`（含 INI `convert`）；TLS1.3 + yamux + 令牌鉴权 + 重连 + 心跳；client / edge 模式、`verify`、frpc/frps 别名；TCP/UDP/HTTP/HTTPS、STCP/SUDP、TCPMUX `httpconnect`，内存基线（见 `tools/bench/README.md`）。
  代理类型：**tcp / udp / http（逐请求按 Host + 路径前缀 + routeByHTTPUser 选路，通配、子域名、Host 改写、Basic Auth、requestHeaders）/ https（SNI 透传）**，独立与受管模式都支持。
  **已实现**：stcp / sudp 与访问者（独立模式用 `[[visitors]]`；受管模式在客户端详情页建「访问者」，目标是同一节点上其他设备的 stcp/sudp 隧道，密钥由 Controller 代为出示，不经 REST 返回；Visitor 可选加密/压缩）、TCP/TCPMUX/Visitor 工作流 AES-256-GCM 与有界 LZ4、按客户端/隧道/UDP peer/SUDP Visitor workflow 连接上限、独立模式本地插件（`socks5` / `http_proxy` / `static_file` / `unix_domain_socket` / `http2https` / `https2http` / `https2https` / `tls2raw`；`virtual_net` 等其余类型进入兼容报告并跳过；`*2*` 类只支持 HTTP/1.1）、健康检查与负载均衡组、HTTP 响应头、PROXY protocol v1/v2、令牌桶限速。TCPMUX passthrough、Visitor transforms、UDP/SUDP peer/session 限制均有回环集成测试。
  **保留边界**：XTCP 与其他兼容报告内标出的未实现插件/字段；`reload` / `status` / `stop` 独立客户端管理接口、frps dashboard、独立 `webServer.tls` / `assetsDir` 均已实现；真实公网部署留待部署验收。`service install|uninstall|status|start|stop` 命令已实现，真实服务管理器实机验收仍待做。
- P02 Controller ✔（功能代码）：SQLite、管理员 CLI、接入令牌、设备管理、隧道 CRUD 与批量、配置热下发、审计、REST、面板托管、API Token、流量分钟桶与 24h/7d 历史、有界日志环、配置导入预览/导出、在线备份、有界保留清理、脚本托管。
  **独立节点**已实现（`RUSTFRP_NODE_ADDR` 开启节点端口；`rustfrp-agent --mode edge --controller … --enroll-token … --public-host …`，见 `docs/NODES.md`），AT-05 在同机全栈测试中通过；内置节点仍与 Controller 同进程。
  `RUSTFRP_BUILTIN_EDGE=off` 可只做管理面；`RUSTFRP_METRICS_TOKEN` 开启 Prometheus `/metrics`；客户端标签与远程重启（退出码 75，需系统服务托管）已实现。
  在线设备跨节点迁移、保留期配置与 Agent 更新命令已实现；跨主机/WAN 迁移和控制面 HTTPS 公网部署验收保留。设备详情的最近登录 IP 由 Edge 填写，覆盖客户端自报值。
- P03 Flutter ✔（功能代码）：登录、概览（含流量图）、节点、客户端、隧道 CRUD/模板/批量、接入向导、审计、设置（主题/令牌/API Token/管理员）、日志、宽窄屏、深浅色；Web 同源托管并在严格 CSP 下实测。
  桌面：窗口壳、管理员控制台模式、受管本机模式、独立模式完整 TOML/YAML/JSON 文本编辑（校验后暂存/原子保存）、日志/诊断、托盘、自启动、通知、CA-07 Agent 更新与 D-09 Flutter App 更新代码均已实现并有自动化测试。三平台真实主机验收保留。
- P07 官网与 Release 构建/签名工作流已实现；官网公网部署、真实签名 Release 所需密钥、公证和正式安全审计报告未完成。
- 当前验收：`make lint`、`make cov` 已通过；Core 覆盖率 99.03/97.97/97.16、Agent 97.08/91.40/91.37、Server 97.16/95.79/92.88，Flutter 行覆盖率 97.26，均超过统一的 90% 最低门槛。其余明确验收保留项见 `docs/IMPLEMENTATION_STATUS.md`。
