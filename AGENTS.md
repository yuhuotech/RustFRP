# AGENTS.md — RustFRP 协作约定

> 给人和 AI 编程助手共同使用。**开始任何工作前先读：** `docs/RUSTFRP_TECH_DESIGN_V1.0.md` §0（两条最高原则）与 `docs/DEVELOPMENT_STANDARDS.md`。

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

Flutter：颜色、字号、圆角沿用 linkory 的 token / 共享控件，action 主色为草绿（`rustfrp-app/lib/theme/tokens.dart`）。**按用户 2026-10-11 的明确要求，宽屏采用两栏布局：可折叠菜单（展开 200px / 收起 72px）+ 内容工作区，不再设置中间列表栏。** 菜单行高 28px、文字沿用 13px token，详情与列表在内容区内切换。

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
- P01 隧道核心 ✔：`frpcfg`（含 INI `convert`）；TLS1.3 + yamux + 令牌鉴权 + 重连 + 心跳；client / edge 模式、`verify`、frpc/frps 别名；内存基线（见 `tools/bench/README.md`）。
  代理类型：**tcp / udp / http（逐请求按 Host + 路径前缀 + routeByHTTPUser 选路，通配、子域名、Host 改写、Basic Auth、requestHeaders）/ https（SNI 透传）**，独立与受管模式都支持。
  **已实现**：stcp / sudp 与访问者（独立模式用 `[[visitors]]`；受管模式在客户端详情页建「访问者」，目标是同一节点上其他设备的 stcp/sudp 隧道，密钥由 Controller 代为出示，不经 REST 返回）、独立模式本地插件（`socks5` / `http_proxy` / `static_file` / `unix_domain_socket` / `http2https` / `https2http` / `https2https` / `tls2raw`；`virtual_net` 等其余类型进入兼容报告并跳过；`*2*` 类只讲 HTTP/1.1）、健康检查（tcp / http，客户端探测、状态变化通知 Edge，不健康时不接收访问者）与负载均衡组（tcp 共用端口、http/https 共用域名，健康成员间轮转，组密钥校验）、http 的 `responseHeaders.set`（每个最终响应改写，不可改分帧头）、PROXY protocol v1/v2（客户端写给本地服务）、带宽限制（`transport.bandwidthLimit` + `bandwidthLimitMode`，令牌桶，每代理双向共享；tcp/http/https，udp 不支持并提示；面板有 KB/s 字段）。
  **尚未实现**：xtcp、tcpmux、受管模式下的插件（Controller 暂不存储 / 下发）、加密/压缩、agent `reload`。`service install|uninstall|status|start|stop` 已实现（命令构造已测，真实 systemctl/launchctl/schtasks 未实机验证）。
  配置里出现这些时会在兼容报告 / 日志中逐代理列出并跳过。
- P02 Controller ✔（部分）：SQLite、管理员 CLI、接入令牌、设备管理、隧道 CRUD 与批量、配置热下发、审计、REST、面板托管、API Token、流量分钟桶与 24h/7d 历史、有界日志环、配置导入预览/导出、在线备份、有界保留清理、脚本托管。
  **独立节点**已实现（`RUSTFRP_NODE_ADDR` 开启节点端口；`rustfrp-agent --mode edge --controller … --enroll-token … --public-host …`，见 `docs/NODES.md`），AT-05 在同机全栈测试中通过；内置节点仍与 Controller 同进程。
  `RUSTFRP_BUILTIN_EDGE=off` 可只做管理面；`RUSTFRP_METRICS_TOKEN` 开启 Prometheus `/metrics`；客户端标签与远程重启（退出码 75，需系统服务托管）已实现。
  **缺口**：设备跨节点迁移、流量/会话保留天数配置化、控制面 HTTPS 部署闭环。设备详情已显示最近登录 IP（由 Edge 填写，覆盖客户端自报值；外置节点经 `NodeMsg::Online.ip` 上报）。
- P03 Flutter ✔（部分）：登录、概览（含流量图）、节点、客户端、隧道 CRUD/模板/批量、接入向导、审计、设置（主题/令牌/API Token/管理员）、日志、宽窄屏、深浅色；Web 同源托管并在严格 CSP 下实测。
  桌面：窗口壳、管理员控制台模式、受管「本机模式」（接入、持久身份、回环控制、本机隧道列表/日志/诊断导出；macOS 实机通过）。
  **缺口（P05）**：托盘、开机启动、通知、本地配置编辑、独立模式入口、应用内更新、内置 agent、三平台安装包；Linux / Windows 后台进程未实机验收。
- P07 官网、Release 流水线、签名、安全审计报告 ✘。
