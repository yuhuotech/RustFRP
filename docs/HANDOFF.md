# RustFRP 交接文档

> 更新于 2026-10-13。进度的权威清单是 `docs/IMPLEMENTATION_STATUS.md`，协作规则见 `AGENTS.md`。本文记录本轮功能实现、整体验收结果及剩余条件。

本轮已独立部署两台用户主机与 `https://rustfrp.dev99.cn`，完成 Linux Agent/Controller、Web 管理台、跨主机迁移和离线恢复。新增回归修复 CLI 管理命令默认模式、TCPMUX 兼容报告误报，缩小双向缓冲通过 Linux 10 MiB 内存门槛。端口、凭据位置、测试证据、服务和剩余验收见 [双主机验收记录](WAN_ACCEPTANCE_20261013.md)。

## 1. 仓库状态

- 本文随本轮功能实现、整体验收结果及进度文档同步更新；具体代码版本以 `main` 最新提交为准。
- 根 `README.md` 是面向使用者的进度摘要；`docs/IMPLEMENTATION_STATUS.md` 是逐项验收权威清单，二者与本文需保持一致。

## 2. 开工前先读

1. `AGENTS.md`：两条最高原则（极致省内存、与 frp 配置完全兼容）、覆盖率与 lint 门槛。
2. `docs/RUSTFRP_TECH_DESIGN_V1.0.md` §0 与 `docs/DEVELOPMENT_STANDARDS.md`。
3. `docs/IMPLEMENTATION_STATUS.md`：逐项验收清单。

## 3. 常用命令

```sh
make lint          # fmt --check + clippy -D warnings
make test          # cargo test --workspace
make cov           # core / agent / server / app 覆盖率并校验基线
make bench-mem     # 内存基准（上限 10 MB）
make app-test      # flutter analyze + test
make app-cov       # Flutter 行覆盖率
make e2e           # 全栈冒烟
```

Flutter 测试偶尔超过 2 分钟，可在 `rustfrp-app/` 下直接 `flutter test`。
CI 将 Rust crates 与 Flutter 的覆盖率校验放在各自的 Job；`make cov` 在本地依次覆盖 Rust crates 和 Flutter。

## 4. 架构速览

| 目录 | 职责 |
|---|---|
| `rustfrp-core` | 线协议（`wire`）、yamux（`mux`）、TLS、身份、`frpcfg` 配置兼容层、`http1` 解析、`pipe`（计数 / 限速）、`node` 节点协议 |
| `rustfrp-agent` | 同一二进制的 client / edge / 受管 / 外置节点；`plugin/`、`visitor`、`health`、`admin`、`service` |
| `rustfrp-server` | Controller：SQLite（单连接）+ axum REST + 内置 Edge；迁移在 `migrations/` |
| `rustfrp-app` | Flutter 面板 + 桌面壳 |

要点：

- 受管模式下配置经隧道控制流整份下发（`ConfigSet`），代理名即隧道 ID。
- 写操作必须带 `reason` 并与业务同事务写审计。
- 数据库 `user_version` 当前是 **19**（本轮迁移 `0016_connection_limits_updates.sql` 至 `0019_visitor_stream_transforms.sql`）；新增迁移要同步改 `db.rs`、API/维护测试和 `tools/e2e/full_stack.py` 里的断言。
- 外置节点保存本地设备名册，Controller 掉线时数据面继续工作。

## 5. 已实现（本轮及之前）

- **隧道类型**：tcp / udp / http / https / tcpmux（仅 httpconnect）/ stcp / sudp。
- **HTTP**：逐请求选路、locations、routeByHTTPUser、Basic Auth、requestHeaders / responseHeaders、Host 改写、Upgrade 透传。
- **通用选项**：PROXY protocol v1/v2、限速（客户端 / 服务端，双向共享令牌桶）、TCP 与 Visitor 工作流 AES-256-GCM/有界 LZ4、客户端/隧道/UDP peer/SUDP Visitor workflow 并发上限、健康检查（tcp / http）、负载均衡组。
- **访问者**：独立模式用 `[[visitors]]`；受管模式在客户端详情页创建。
- **插件**：独立与受管模式均支持 socks5、http_proxy、static_file、unix_domain_socket、http2https、https2http、https2https、tls2raw；受管私密字段不经 REST 返回，证书文件在 Agent 本机按路径读取并受 16 KiB 限制，`*2*` 转发仅支持 HTTP/1.1。
- **运维**：独立客户端 `webServer` 管理接口与 `reload` / `status` / `stop`；`service install|…` 系统服务；远程重启（退出码 75，需服务托管）。
- **TCPMUX**：`multiplexer=httpconnect`、CONNECT Host 路由、并发限制与 `tcpmuxPassthrough` 完整 CONNECT 透传均已实现并有真实回环测试。
- **frps dashboard / TLS**：只读 dashboard JSON API、Basic Auth、HTTPS 与 `assetsDir` 安全静态资源已接入；独立 frpc 的 `webServer.tls` / `assetsDir` 也由运行时支持。
- **Controller**：管理员与角色、API Token、流量历史、配置导入导出（含预览/警告/理由确认）、备份与配置化保留清理、标签、`/metrics`、设备登录 IP、外置节点、设备跨节点迁移（迁移理由审计、旧节点清理、目标指纹固定重连）、Agent 更新命令、有界日志尾随。
- **面板**：概览、节点、客户端、隧道（含 TCPMUX、每客户端/隧道连接上限、HTTP 选项、插件 JSON、限速、健康检查、分组）、访问者、审计、设置；桌面本机模式可编辑完整本地 TOML/YAML/JSON 文件并在保存前校验。
- **此前完成**：受管插件存储/校验/下发/导入导出与面板 JSON 配置；配置化审计/流量保留；Controller 和本机日志游标尾随；独立模式桌面入口；桌面托盘、自启动、连接通知和随包 Agent；中文官网、HTTPS 部署模板、多平台 Release 构建及 Ed25519 清单签名工作流。
- **本轮补齐**：TCPMUX passthrough、Visitor 加密/压缩、客户端/隧道/UDP/SUDP 连接限制、CA-07 Agent 签名更新、D-09 Flutter App 更新、Controller 触发的跨 Edge 迁移、桌面完整配置文本编辑、frps dashboard/TLS/assetsDir。指定功能均有自动化回归；真实签名/公网/跨主机/多平台验收仍按下表保留。

## 6. 功能边界与交付缺口

| 项 | 说明 |
|---|---|
| TCPMUX / Visitor 选项 / 连接上限 | TCPMUX passthrough、Visitor `useEncryption/useCompression`、UDP/SUDP `maxConnections` 均已实现并有回环/单元测试；XTCP 属 P2/V1.1 |
| `frps` dashboard、`webServer.tls` / `assetsDir` | Dashboard 鉴权与 Controller 公网 HTTPS 已在 Linux 实机验证；独立 Dashboard TLS/assetsDir 的公网部署仍待验证 |
| 设备跨节点迁移 | 已在两台物理主机通过迁移、指纹重连/持久化、AT-05 和流量回报；NAT 侧节点数据面使用现有 Tailscale WAN 私网 |
| 桌面本地配置编辑 | 完整 TOML/YAML/JSON 文本编辑、Agent 校验、暂存/原子替换和失败保护已实现并有自动化测试；目标主机验收未做 |
| CA-07 / D-09 更新器 | Agent 与 Flutter App 均有 Ed25519 签名清单/版本标记验证、摘要校验、平台安装/回滚逻辑及自动化测试；签名私钥/公钥变量未配置，真实 Release 未发布 |
| 官网与部署 | Linux Controller / Agent / HTTPS 面板已部署；`rustfrp-web/` 官网公网发布仍未完成 |
| 多平台安装、系统集成与安全 | macOS/Linux/Windows 主机上的安装/更新、托盘、自启动、服务仍需真实验收；Apple 签名/公证、真实签名 Release、供应链加固与正式安全审计报告未完成 |

## 7. 质量现状（2026-10-13）

| Crate | 行 | 函数 | 分支 | 90% 最低门槛 |
|---|---:|---:|---:|---|
| core | 99.03% | 97.97% | 97.15% | 通过 |
| agent | 97.08% | 91.41% | 91.52% | 通过 |
| server | 97.16% | 95.79% | 92.88% | 通过 |
| Flutter App | 97.26% | — | — | 通过 |

- `make lint`、`make test`（601 项）、`make app-test`（158 项）、`make web-build`、`make e2e`、`make bench-mem` 和 `make cov` 均通过；每个 crate 的行/函数/分支覆盖率门槛统一为 90%。
- 本机 `flutter build macos --release` 成功，产出 `rustfrp_app.app`（52.2 MB）；该产物不是签名 Release，也不代表 Windows/Linux/macOS Intel 的实机验收。
- `make bench-mem`（macOS Release，10 条隧道）：空闲 Edge/Client 5.05/5.06 MiB，100 条空闲连接 6.97/7.41 MiB；20 次 512 KiB 回显后 7.39/7.88 MiB，关闭后 7.41/7.89 MiB，均低于 10 MiB 上限。
- `make e2e` 使用真实 server/agent 完成受管接入、300000 字节 TCP 回显、流量/日志/隧道列表、重连、UDP、HTTP 域名与子域名、在线备份及审计检查。新增 TCPMUX/加密/压缩、Agent 更新签名流水线、节点迁移与 dashboard 另有集成/单元测试。
- 2026-10-12 的本地基线没有实机部署；2026-10-13 已完成 Linux Agent/systemd、Controller HTTPS 和双主机迁移，仍未发布真实签名 Release，Windows/Linux/macOS Intel Flutter 桌面完整验收保留。
- 本轮指定功能代码与自动化回归完成；覆盖率门槛已统一为 90%，正式安全审计仍未完成。
- 代码里 `tests/` 下的集成测试不计入覆盖率；单元测试模块用 `#[cfg_attr(coverage, coverage(off))]`。

## 8. 已知注意事项

- Linux systemctl / Agent 与双主机公网、Tailscale WAN 节点已实测；launchctl / schtasks 及多平台 Flutter 桌面完整验收仍未完成。
- 本轮网络验收范围和未复测组合见 `WAN_ACCEPTANCE_20261013.md`，不能外推为正式 Release 或全面安全审计。
- `*2*` 类插件只支持 HTTP/1.1，不支持 HTTP/2。
- 为插件启用了 rustls 的 `tls12` 特性（仅应用层配置使用，隧道本身仍是 TLS 1.3）。
- `rustfrp-protocol/fixtures/edge-test-*.pem` 是测试用自签名证书和私钥，已提交。
- 早先一次 `dart format` 让部分没改功能的 Flutter 文件出现了纯格式差异。
- 编辑代码时避免在 `cargo fmt` 之后用「字符串替换」脚本去改同一段代码：格式变了会静默匹配不上。脚本里要断言目标存在。

## 9. 剩余工作（按价值排序）

1. 配置受保护的 `RUSTFRP_RELEASE_SIGNING_KEY_PEM` 与匹配的 `RUSTFRP_RELEASE_PUBLIC_KEY_HEX`，发布并验证真实签名 Release。
2. 发布官网并完成正式安全审计；Controller HTTPS/Cookie 已验。配置下发使用隧道控制流，无 WebSocket。
3. 在 macOS arm64/Intel、Windows x64、Linux x64 主机验收安装/更新/卸载、托盘、自启动、通知、后台 Agent 与系统服务。
4. 补 1000 连接、50 设备、吞吐和延迟专项测试；跨主机迁移、Linux 100 连接内存已验，之后再评估 V1.1 的 XTCP、KCP/QUIC 和协议互通。

## 10. 提交规范

- commit 信息结尾加 `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`。
- 只在被要求时 commit / push。
