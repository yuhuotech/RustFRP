# RustFRP 交接文档

> 写于 2026-10-11。进度的权威清单是 `docs/IMPLEMENTATION_STATUS.md`，协作规则见 `AGENTS.md`。本文只讲「现在在哪、怎么接着干」。

## 1. 仓库状态

- 分支 `main`，功能代码已提交至 `6f209da`；本轮主要功能提交为 `0fd5043` … `6f209da`。
- 本文随交接文档提交保存；具体未推送数量以 `git status --branch` 为准。
- 提交完成后的工作区状态以 `git status --short` 为准。

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
- 数据库 `user_version` 当前是 **14**；新增迁移要同步改 `db.rs` 与 `tests/maintenance.rs` 里的断言。
- 外置节点保存本地设备名册，Controller 掉线时数据面继续工作。

## 5. 已实现（本轮及之前）

- **隧道类型**：tcp / udp / http / https / stcp / sudp。
- **HTTP**：逐请求选路、locations、routeByHTTPUser、Basic Auth、requestHeaders / responseHeaders、Host 改写、Upgrade 透传。
- **通用选项**：PROXY protocol v1/v2、限速（客户端 / 服务端，双向共享令牌桶）、健康检查（tcp / http）、负载均衡组。
- **访问者**：独立模式用 `[[visitors]]`；受管模式在客户端详情页创建。
- **本地插件（仅独立模式）**：socks5、http_proxy、static_file、unix_domain_socket、http2https、https2http、https2https、tls2raw。
- **运维**：独立客户端 `webServer` 管理接口与 `reload` / `status` / `stop`；`service install|…` 系统服务；远程重启（退出码 75，需服务托管）。
- **Controller**：管理员与角色、API Token、流量历史、配置导入导出、备份与保留清理、标签、`/metrics`、设备登录 IP、外置节点。
- **面板**：概览、节点、客户端、隧道（含 HTTP 选项、限速、健康检查、分组）、访问者、审计、设置。

## 6. 未实现 / 缺口

| 项 | 说明 |
|---|---|
| tcpmux、xtcp | 配置会被识别并进入兼容报告，运行时没有 |
| 受管模式的插件 | Controller 不存、不下发，面板无表单；导入时标记「保留但未激活」 |
| 加密 / 压缩 | `transport.useEncryption` / `useCompression` 仅提示 |
| frps dashboard、`webServer.tls` / `assetsDir` | 启动日志提示未实现 |
| 设备跨节点迁移 | 设备要换节点需要重新接入 |
| 官网 `rustfrp-web/`、`deploy/` 模板 | 未建 |
| 桌面托盘、开机启动、通知、应用内更新、三平台安装包 | P05 |
| 发布 / 签名流水线、控制面 HTTPS 部署闭环 | P07 |

## 7. 质量现状（2026-10-11 最近一次全量）

| Crate | 行 | 函数 | 分支 |
|---|---|---|---|
| core | 99.27% | 98.28% | 97.44% |
| agent | 98.11% | 95.54% | 91.50% |
| server | 96.93% | 95.65% | 93.14% |

- Flutter 行覆盖率 97.94%。
- 内存基准：100 个空闲连接约 7.2 MB（上限 10 MB），比早期 6.5 MB 有所上涨，继续加功能前留意。
- **agent 的行 / 分支覆盖率贴着基线**（`tools/cov/baseline.json`，容差 0.05 个百分点）。新增代码必须带测试，否则 `make cov` 会失败。
- 代码里 `tests/` 下的集成测试不计入覆盖率；单元测试模块用 `#[cfg_attr(coverage, coverage(off))]`。

## 8. 已知注意事项

- 没有在真实 systemctl / launchctl / schtasks、Windows、多机公网环境验证过。
- 节点相关测试只在本机回环上跑过。
- `*2*` 类插件只支持 HTTP/1.1，不支持 HTTP/2。
- 为插件启用了 rustls 的 `tls12` 特性（仅应用层配置使用，隧道本身仍是 TLS 1.3）。
- `rustfrp-protocol/fixtures/edge-test-*.pem` 是测试用自签名证书和私钥，已提交。
- 早先一次 `dart format` 让部分没改功能的 Flutter 文件出现了纯格式差异。
- 编辑代码时避免在 `cargo fmt` 之后用「字符串替换」脚本去改同一段代码：格式变了会静默匹配不上。脚本里要断言目标存在。

## 9. 建议的下一步（按价值排序）

1. 受管模式的插件：Controller 存储 / 校验 / 下发 / 导入导出 + 面板表单（模式可参考健康检查与限速那几条链路）。
2. 控制面 HTTPS 部署闭环与 `deploy/` 模板。
3. 设备跨节点迁移（需要让设备持久化新的接入地址与证书指纹）。
4. 桌面端托盘 / 开机启动 / 通知，三平台实机验收。
5. 发布与签名流水线、官网。
6. tcpmux、xtcp（用得少，放最后）。

## 10. 提交规范

- commit 信息结尾加 `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`。
- 只在被要求时 commit / push。
