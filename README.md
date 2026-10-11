# RustFRP

RustFRP 是一个自托管内网穿透系统，包含 Rust 数据面与 Controller、Flutter Web 管理面板和桌面端。配置层以 frp 配置为兼容目标；RustFRP 的线协议与官方 frp 不互通，迁移时两端都要使用 RustFRP。

## 当前进度（2026-10-12）

**V1.0 尚未全部完成，当前工作区包含本轮实现但尚未发布。** 已有 TCP/UDP/HTTP/HTTPS、STCP/SUDP、受管与独立模式、本地/受管插件、健康检查、负载均衡、限速、PROXY protocol、Controller 多节点、流量与日志、配置导入导出、备份及配置化数据保留。桌面托盘、自启动、通知、内置 Agent、独立配置入口，多平台构建工作流、官网源码和 HTTPS 部署模板已加入。

### 仍未完成的需求与交付

- **TCPMUX**（V1.0 P1）、传输 `useEncryption` / `useCompression` 和按客户端/隧道的连接数限制仍未实现；配置会进入兼容报告。
- **桌面自动更新器**（CA-07）未实现：缺少可信公钥固定、签名校验、原子替换、回滚和 Agent/App 协调升级。
- **真实三平台验收**未完成：CI 尚未实际产出并安装 macOS、Windows、Linux 发布包；托盘、自启动、通知、后台 Agent 与系统服务仍需目标主机验证。
- Release 的 Apple 签名/公证与 Ed25519 清单签名需要外部 GitHub Actions 凭据；官网尚未部署，正式域名、证书和反向代理也尚未实地验收。
- **安全审计报告**、安装器/更新器对可信发布公钥的校验闭环尚未完成。
- 设备跨节点迁移、完整的桌面本地配置逐项编辑仍未完成；frps dashboard、`webServer.tls` 与 `assetsDir` 运行支持也未实现，均保留兼容提示。

XTCP/P2P、KCP/QUIC、frp 线协议互通、移动端、ACME、多租户计费和 Webhook 告警属于需求中的 V1.1/V2 范围，不计入本次 V1.0 完成声明。

## 本轮验收（2026-10-12）

`make lint`、`make test`、`make cov`、Flutter analyze 与 132 项测试、`make app-cov`、`make web-build`、`make e2e`、`make bench-mem` 均通过。`make cov` 按仓库覆盖率检查器通过：

| Crate | 行覆盖率 | 函数覆盖率 | 分支覆盖率 |
|---|---:|---:|---:|
| rustfrp-core | 99.19% | 98.04% | 97.44% |
| rustfrp-agent | 98.10% | 95.40% | 91.49% |
| rustfrp-server | 97.00% | 95.65% | 93.37% |
| Flutter App | 97.28% | — | — |

Agent 行覆盖率比其 98.14% 基线低 0.04 个百分点；仓库检查器设置 0.05 个百分点容差，因此检查通过。100 条空闲连接 Release RSS 为 Edge 6.78 MiB / Client 7.12 MiB；20 次 512 KiB 回显后的峰值为 7.12 / 7.69 MiB，关闭后 7.14 / 7.70 MiB，低于 10 MiB 上限。真实全栈 e2e 完成 300000 字节回显、管理 API、重连、UDP/HTTP 路由和在线备份检查。Web 与 macOS ARM64 Release 构建成功；Linux/Windows 安装、macOS Intel 安装、真实托盘/系统服务、签名公证与正式 Release 尚未完成。

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
