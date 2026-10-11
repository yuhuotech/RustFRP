# RustFRP 交接文档

> 更新于 2026-10-12。进度的权威清单是 `docs/IMPLEMENTATION_STATUS.md`，协作规则见 `AGENTS.md`。本文说明当前工作区状态、明确缺口和接手顺序。

## 1. 仓库状态

- 当前工作区有本轮实现和文档改动，尚未提交；提交/推送状态以 `git status --short --branch` 为准。
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
- 数据库 `user_version` 当前是 **15**（迁移 `0015_managed_plugins.sql`）；新增迁移要同步改 `db.rs` 与 `tests/maintenance.rs` 里的断言。
- 外置节点保存本地设备名册，Controller 掉线时数据面继续工作。

## 5. 已实现（本轮及之前）

- **隧道类型**：tcp / udp / http / https / stcp / sudp。
- **HTTP**：逐请求选路、locations、routeByHTTPUser、Basic Auth、requestHeaders / responseHeaders、Host 改写、Upgrade 透传。
- **通用选项**：PROXY protocol v1/v2、限速（客户端 / 服务端，双向共享令牌桶）、健康检查（tcp / http）、负载均衡组。
- **访问者**：独立模式用 `[[visitors]]`；受管模式在客户端详情页创建。
- **插件**：独立与受管模式均支持 socks5、http_proxy、static_file、unix_domain_socket、http2https、https2http、https2https、tls2raw；受管私密字段不经 REST 返回，证书文件在 Agent 本机按路径读取并受 16 KiB 限制，`*2*` 转发仅支持 HTTP/1.1。
- **运维**：独立客户端 `webServer` 管理接口与 `reload` / `status` / `stop`；`service install|…` 系统服务；远程重启（退出码 75，需服务托管）。
- **Controller**：管理员与角色、API Token、流量历史、配置导入导出（含预览/警告/理由确认）、备份与配置化保留清理、标签、`/metrics`、设备登录 IP、外置节点、有界日志尾随。
- **面板**：概览、节点、客户端、隧道（含 HTTP 选项、插件 JSON、限速、健康检查、分组）、访问者、审计、设置；桌面本机模式可选受管或独立 TOML。
- **本轮新增**：受管插件存储/校验/下发/导入导出与面板 JSON 配置；配置化审计/流量保留；Controller 和本机日志游标尾随；独立模式桌面入口；桌面托盘、自启动、连接通知和随包 Agent；中文官网、HTTPS 部署模板、多平台 Release 构建及 Ed25519 清单签名工作流。

## 6. 未实现 / 缺口

| 项 | 说明 |
|---|---|
| TCPMUX、xtcp | TCPMUX 是 V1.0 P1 缺口，尚无运行时；XTCP 属 P2/V1.1。二者在兼容报告中明确列出 |
| 传输加密 / 压缩、连接数限制 | 尚无运行时实现；加密/压缩会进入兼容报告，连接数限制仍缺 |
| `frps` dashboard、`webServer.tls` / `assetsDir` | 配置能力未实现并有提示；管理面 HTTPS 用反向代理部署模板，需真实部署验收 |
| 设备跨节点迁移、桌面完整本地配置编辑 | 尚未完成 |
| 官网与部署 | `rustfrp-web/`、Caddy/nginx/Compose/systemd 模板已加入，尚未部署 |
| 桌面 | 托盘、关闭到托盘、自启动、状态通知、独立配置入口、随包 Agent 已实现并有自动化测试；三平台实机验收仍缺 |
| 自动更新 | CA-07 更新器未实现：可信公钥固定、签名校验、原子替换、回滚、App/Agent 协调升级均缺 |
| 发布 / 签名 | 多平台 Release 工作流和 Ed25519 SHA256 清单签名已写；尚未运行真实 Release。Apple 签名/公证和清单签名需要 CI 凭据；安装器/更新器尚未固定可信公钥，正式安全审计报告未出 |

## 7. 质量现状（2026-10-12 最终联调）

| Crate | 行 | 函数 | 分支 |
|---|---|---|---|
| core | 99.19% | 98.04% | 97.44% |
| agent | 98.10% | 95.40% | 91.49% |
| server | 97.00% | 95.65% | 93.37% |

- Flutter 行覆盖率 97.28%，132 项测试通过。
- `make lint`、`make test`、`make cov`、Flutter analyze、132 项 Flutter 测试/`make app-cov`、`make web-build`、`make e2e` 与 `make bench-mem` 均通过；Compose、Release workflow YAML 与发布校验脚本语法检查通过。
- 覆盖率检查器对既有基线允许 0.05 个百分点误差。Agent 行覆盖率 98.10%，比 98.14% 基线低 0.04 个百分点，因此检查器通过；函数 / 分支均高于基线。其他 crate 达到或超过基线。
- `make bench-mem`（macOS Release，10 条隧道）结果：100 条空闲连接 Edge/Client 6.78/7.12 MiB；20 次 512 KiB 回显后的峰值 Edge/Client 7.12/7.69 MiB，连接关闭后 7.14/7.70 MiB，均低于 10 MiB 上限。
- `make e2e` 使用真实 server/agent 完成受管接入、300000 字节 TCP 回显、流量/日志/隧道列表、重连、UDP、HTTP 域名与子域名、在线备份及审计检查。
- 本机 macOS ARM64 Release `.app` 构建成功（51.5 MB）；Linux、Windows、macOS Intel 发布包未在真实主机完成安装验收，Release 签名/公证未运行。
- `make cov` 与 `make app-cov` 不会降低基线；新增代码仍须带有有效断言的测试。
- 代码里 `tests/` 下的集成测试不计入覆盖率；单元测试模块用 `#[cfg_attr(coverage, coverage(off))]`。

## 8. 已知注意事项

- 没有在真实 systemctl / launchctl / schtasks、Windows/Linux 桌面主机或多机公网环境验证过；当前自动化测试不能替代这些验收。
- 节点相关测试只在本机回环上跑过。
- `*2*` 类插件只支持 HTTP/1.1，不支持 HTTP/2。
- 为插件启用了 rustls 的 `tls12` 特性（仅应用层配置使用，隧道本身仍是 TLS 1.3）。
- `rustfrp-protocol/fixtures/edge-test-*.pem` 是测试用自签名证书和私钥，已提交。
- 早先一次 `dart format` 让部分没改功能的 Flutter 文件出现了纯格式差异。
- 编辑代码时避免在 `cargo fmt` 之后用「字符串替换」脚本去改同一段代码：格式变了会静默匹配不上。脚本里要断言目标存在。

## 9. 建议的下一步（按价值排序）

1. 先补 V1.0 P1：TCPMUX、按客户端/隧道连接数限制、传输加密/压缩（逐项依据上游行为实现或显式确认边界）。
2. 完成桌面本地配置编辑与 CA-07 更新器：可信公钥固定、签名校验、回滚和 Agent/App 协调更新。
3. 用 macOS arm64/Intel、Windows x64、Linux x64 真实主机验收桌面托盘、自启动、通知、后台 Agent、安装/卸载；修正发布包后再跑签名/公证 Release。
4. 将官网部署到正式域名，按 `deploy/README.md` 完成反向代理 HTTPS、Cookie 与 WebSocket 的生产验收。
5. 做跨节点迁移、安全审计与完整性能/内存回归；然后再评估 V1.1 的 XTCP、KCP/QUIC 和协议互通。

## 10. 提交规范

- commit 信息结尾加 `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`。
- 只在被要求时 commit / push。
