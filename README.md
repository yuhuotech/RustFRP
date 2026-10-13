# RustFRP

RustFRP 是一个自托管内网穿透系统，包含 Rust 数据面与 Controller、Flutter Web 管理面板和桌面端。配置层以 frp 配置为兼容目标；RustFRP 的线协议与官方 frp 不互通，迁移时两端都要使用 RustFRP。

## 当前进度（2026-10-13）

已在 `182.254.220.82` / `172.17.32.251` 独立部署 Linux Controller、Agent 和公网 HTTPS 面板（`https://rustfrp.dev99.cn`），原 frp 不受影响。受管/独立隧道、8 类插件、管理操作、节点跨主机迁移、控制面离线恢复和 Linux systemd 通过真实联调；端口、证据与明确未验收项见 [双主机验收记录](docs/WAN_ACCEPTANCE_20261013.md)。

**本轮指定的遗留功能已全部完成代码实现，并通过自动化回归；V1.0 整体仍未完成交付验收。** 既有 TCP/UDP/HTTP/HTTPS、STCP/SUDP、受管与独立模式、插件、健康检查、负载均衡、限速、PROXY protocol、Controller 多节点、流量与日志、配置导入导出、备份和配置化数据保留均已实现。

### 已完成的遗留功能

- TCPMUX `httpconnect` 支持 passthrough，可配置为把完整 CONNECT 请求转发给目标服务。
- TCP 与 Visitor 工作流可选 AES-256-GCM 加密和有界 LZ4 压缩。
- 并发限制覆盖客户端、隧道、UDP 活跃源地址和 SUDP Visitor 工作流；面板与 API 可配置。
- CA-07 Agent 与 D-09 Flutter 桌面 App 更新器支持 Ed25519 签名清单/版本标记校验、摘要校验、下载进度和失败保护。
- 在线设备跨 Edge 迁移具备审计、旧节点清理、目标证书指纹固定、自动重连和目标持久化。
- 独立桌面模式可完整编辑 TOML/YAML/JSON 配置文件，保存前用 Agent 校验并原子替换。
- frps dashboard、`webServer.tls`、`assetsDir` 与独立 frpc 管理 WebServer TLS 已接入运行时。

### 仍未完成的验收

- 真实签名 Release 所需私钥与 `RUSTFRP_RELEASE_PUBLIC_KEY_HEX` 尚未配置；CA-07/D-09 会在缺少可信公钥时 fail closed。
- 官网公网部署、Windows/Linux/macOS Intel Flutter 桌面完整实机验收仍未完成；Controller HTTPS、Linux Agent 服务和跨主机迁移已通过本轮指定环境验收。
- 正式安全审计报告尚未出具；XTCP/P2P、KCP/QUIC、frp 线协议互通、移动端、ACME、多租户计费和 Webhook 告警属于 V1.1/V2。

XTCP/P2P、KCP/QUIC、frp 线协议互通、移动端、ACME、多租户计费和 Webhook 告警属于需求中的 V1.1/V2 范围，不计入本次 V1.0 完成声明。

## 本轮整体验收（2026-10-13）

`make lint`、`make test`（601 项）、`make app-test`（158 项）、`make web-build`、`make e2e`、`make bench-mem` 和 `make cov` 通过。覆盖率统一按每 crate 各指标至少 90% 验收，不再要求追平历史高点。覆盖率如下：

| Crate | 行覆盖率 | 函数覆盖率 | 分支覆盖率 |
|---|---:|---:|---:|
| rustfrp-core | 99.03% | 97.97% | 97.15% |
| rustfrp-agent | 97.08% | 91.41% | 91.52% |
| rustfrp-server | 97.16% | 95.79% | 92.88% |
| Flutter App | 97.26% | — | — |

Core、Agent、Server 的行/函数/分支及 Flutter 行覆盖率均达到 90% 门槛。双向每方向 4 KiB 缓冲后，Linux 10 隧道/100 连接 RSS 峰值 Edge/Client 8.91/9.46 MiB，macOS 为 6.64/7.19 MiB，均低于 10 MiB。

权威的逐项状态、质量门槛和验收边界见 [`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md)；接手说明见 [`docs/HANDOFF.md`](docs/HANDOFF.md)。

## 开发与部署

常用开发命令：

```sh
make lint
make test
make cov
make bench-mem
make app-test
make app-cov
make web-build
make e2e
```

生产 HTTPS 反向代理与容器/系统服务样板见 [`deploy/README.md`](deploy/README.md)。静态官网源码见 [`rustfrp-web/`](rustfrp-web/)。Release 工作流及所需凭据见 [`tools/release/README.md`](tools/release/README.md)。

配置兼容、安装、TLS、备份和节点文档集中在 [`docs/`](docs/)；仓库协作与实现门槛见 [`AGENTS.md`](AGENTS.md)。
