# RustFRP

RustFRP 是一个自托管内网穿透系统，包含 Rust 数据面与 Controller、Flutter Web 管理面板和桌面端。配置层以 frp 配置为兼容目标；RustFRP 的线协议与官方 frp 不互通，迁移时两端都要使用 RustFRP。

## 当前进度（2026-10-12）

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
- 官网/Controller 公网部署、跨主机迁移、Windows/Linux/macOS Intel 实机验收仍未完成。
- Agent 覆盖率为行/函数/分支 **97.07% / 91.40% / 91.29%**；行/函数仍低于历史基线 **98.14% / 95.36%**，分支已超过 91.10% 基线；Core、Server、Flutter 已达基线。
- 正式安全审计报告尚未出具；XTCP/P2P、KCP/QUIC、frp 线协议互通、移动端、ACME、多租户计费和 Webhook 告警属于 V1.1/V2。

XTCP/P2P、KCP/QUIC、frp 线协议互通、移动端、ACME、多租户计费和 Webhook 告警属于需求中的 V1.1/V2 范围，不计入本次 V1.0 完成声明。

## 本轮整体验收（2026-10-12）

`make lint`、`make test`、`make app-test`（158 项）、`make web-build`、`make e2e` 和 `make bench-mem` 通过。覆盖率如下：

| Crate | 行覆盖率 | 函数覆盖率 | 分支覆盖率 |
|---|---:|---:|---:|
| rustfrp-core | 99.03% | 97.97% | 97.16% |
| rustfrp-agent | 97.07% | 91.40% | 91.29% |
| rustfrp-server | 97.16% | 95.80% | 92.88% |
| Flutter App | 97.30% | — | — |

Core 行/函数/分支为 99.03% / 97.97% / 97.16%。Server 和 Flutter 达到历史基线；Agent 高于 90% 最低门槛但未达到历史基线，因此 `make cov` 仍失败。100 条空闲连接 Release RSS 为 Edge/Client 6.97/7.41 MiB；20 次 512 KiB 回显后为 7.39/7.88 MiB，关闭后为 7.41/7.89 MiB，均低于 10 MiB。

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
