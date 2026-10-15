# RustFRP

RustFRP 是一个自托管内网穿透系统，包含 Rust 数据面与 Controller、共享原生 Web 管理面和 Tauri 桌面端。Web 与桌面共用 `rustfrp-ui/` 的 HTML/CSS/TypeScript 页面；桌面通过 Tauri typed IPC 调用 Rust 本机能力。配置层以 frp 配置为兼容目标；RustFRP 的线协议与官方 frp 不互通，迁移时两端都要使用 RustFRP。

## 当前进度（2026-10-15）

已在 `182.254.220.82` / `172.17.32.251` 独立部署 Linux Controller、Agent 和公网 HTTPS 面板（`https://rustfrp.dev99.cn`），原 frp 不受影响。受管/独立隧道、8 类插件、管理操作、节点跨主机迁移、控制面离线恢复和 Linux systemd 通过真实联调；端口、证据与明确未验收项见 [双主机验收记录](docs/WAN_ACCEPTANCE_20261013.md)。

**管理 UI 已切换为 Web 与 Tauri 桌面共用的原生 HTML/CSS/TypeScript。** 本地 macOS 与 Linux ARM64 虚拟机的桌面 E2E、三引擎浏览器 E2E、全栈验收和所有覆盖率门槛均通过；Flutter 工程、依赖和构建入口已删除。Windows/物理 Linux 实机、桌面进程树内存对照和真实签名 Release 尚未取得；macOS 窗口恢复 p95 为 532ms，高于计划建议的 200ms。逐项证据见 [`UI 重构验收清单`](docs/UI_REFACTOR_STATUS.md)。

### 已完成的遗留功能

- TCPMUX `httpconnect` 支持 passthrough，可配置为把完整 CONNECT 请求转发给目标服务。
- TCP 与 Visitor 工作流可选 AES-256-GCM 加密和有界 LZ4 压缩。
- 并发限制覆盖客户端、隧道、UDP 活跃源地址和 SUDP Visitor 工作流；面板与 API 可配置。
- CA-07 Agent 与桌面 App 更新器支持 Ed25519 签名清单/版本标记校验、摘要校验、下载进度和失败保护。
- 在线设备跨 Edge 迁移具备审计、旧节点清理、目标证书指纹固定、自动重连和目标持久化。
- 独立桌面模式可完整编辑 TOML/YAML/JSON 配置文件，保存前用 Agent 校验并原子替换。
- frps dashboard、`webServer.tls`、`assetsDir` 与独立 frpc 管理 WebServer TLS 已接入运行时。

### 仍未完成的验收

- 真实签名 Release 所需私钥与 `RUSTFRP_RELEASE_PUBLIC_KEY_HEX` 尚未配置；更新器在缺少可信公钥时 fail closed。
- 官网公网部署、Windows/Linux/macOS 多平台桌面安装和系统能力完整实机验收仍未完成；Controller HTTPS、Linux Agent 服务和跨主机迁移已通过指定环境验收。
- 正式安全审计报告尚未出具；XTCP/P2P、KCP/QUIC、frp 线协议互通、移动端、ACME、多租户计费和 Webhook 告警属于 V1.1/V2。

XTCP/P2P、KCP/QUIC、frp 线协议互通、移动端、ACME、多租户计费和 Webhook 告警属于需求中的 V1.1/V2 范围，不计入本次 V1.0 完成声明。

## 既有数据面验收记录（2026-10-13）

以下是 UI 重构前的数据面验收记录，不代表当前 UI 重构代码已通过最终验收。当前 UI 与桌面门槛、命令和实测结果以 [`docs/UI_REFACTOR_STATUS.md`](docs/UI_REFACTOR_STATUS.md) 为准。

| Crate | 行覆盖率 | 函数覆盖率 | 分支覆盖率 |
|---|---:|---:|---:|
| rustfrp-core | 99.03% | 97.97% | 97.15% |
| rustfrp-agent | 97.08% | 91.41% | 91.52% |
| rustfrp-server | 97.16% | 95.79% | 92.88% |
| 共享静态资源 crate | 97.37% | 100% | 90% |

Core、Agent、Server 和静态资源的历史行/函数/分支覆盖率达到 90% 门槛。双向每方向 4 KiB 缓冲后，Linux 10 隧道/100 连接 RSS 峰值 Edge/Client 8.91/9.46 MiB，macOS 为 6.64/7.19 MiB，均低于 10 MiB。

权威的逐项状态、质量门槛和验收边界见 [`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md)；接手说明见 [`docs/HANDOFF.md`](docs/HANDOFF.md)。

## 开发与部署

常用开发命令：

```sh
make lint
make test
make cov
make bench-mem
make ui-test
make ui-cov
make desktop-test desktop-cov desktop-e2e
make ui-e2e
make web-build
make e2e
```

本地预览共享管理界面：

```sh
./start_dev.sh
```

浏览器打开 <http://localhost:9674>。macOS 桌面体验先运行 `make desktop-build`，再打开 `rustfrp-desktop/target/release/bundle/macos/RustFRP.app`，Controller 地址使用 `http://127.0.0.1:9674`。预览服务默认把 Web、内置 Edge 和独立节点接入分别绑定在 `9674`、`127.0.0.1:17000`、`127.0.0.1:17001`；结束后运行 `./start_dev.sh stop`。

桌面本机模式可直接点击「登录管理服务」，登录页「使用本机管理服务」一键填写本地地址。管理员登录后从「本机模式 → 连接此电脑」选择节点并启动，应用自动处理地址、证书和一次性接入凭据。手动连接、已有 frpc 配置与高级设置仍可使用，详见 [本机模式说明](docs/LOCAL_AGENT.md)。

生产 HTTPS 反向代理与容器/系统服务样板见 [`deploy/README.md`](deploy/README.md)。静态官网源码见 [`rustfrp-web/`](rustfrp-web/)。Release 工作流及所需凭据见 [`tools/release/README.md`](tools/release/README.md)。

配置兼容、安装、TLS、备份和节点文档集中在 [`docs/`](docs/)；仓库协作与实现门槛见 [`AGENTS.md`](AGENTS.md)。
