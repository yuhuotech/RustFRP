# 管理面板 UI 部署记录（2026-10-13）

历史记录：该次 Web 发布使用 Flutter 构建产物，保留用于说明当时的部署与回退结果。当前 Web/桌面共用 `rustfrp-ui/`；本文件不是当前构建步骤。

公网入口：<https://rustfrp.dev99.cn>。本轮部署包含紧凑概览卡、管理表格、详情分组、彩色状态、菜单名称、160px 侧栏、Logo 对齐以及增强选中样式。

## 目标与实际变更

- `ubuntu@182.254.220.82`：更新 Controller 托管的 Web 静态资源。`/opt/rustfrp-test/panel` 已切换为指向 `/opt/rustfrp-test/panel-releases/ui-20261013-69a699c0124f` 的符号链接。
- `ubuntu@172.17.32.251`：运行客户端和独立 Edge，没有 Web 面板，本轮没有替换二进制或重启服务，已检查在线状态与跨主机转发。
- Controller、两台主机上的 Edge/Client、原 frps/frpc 均未重启；配置、数据库、端口与 nginx 配置未修改。`9674` 仍仅是本地开发端口，公网部署沿用 HTTPS 与回环管理端口 `28080`。
- 上传包校验 SHA256，并逐一核对 52 个静态文件。部署包中的 `index.html` 与 `flutter_bootstrap.js` 加入版本查询参数，避免缓存旧引导脚本及 `main.dart.js`；源码与本地构建目录未因此修改。

## 产物与回退

- 发布标识：`ui-20261013-69a699c0124f`。
- `main.dart.js` SHA256：`69a699c0124f9da5157dab368c4e3c3c0497d5c8d924429c360a1eb154e3c0f2`。
- 上传包 SHA256：`8f5df6aa9d75f5350e7a7d7b047df6b9bbb74c1d23e08fd5e13994d2302aa1f8`。
- 原面板完整保存在 `/opt/rustfrp-test/panel-before-ui-20261013-69a699c0124f`。
- 本地部署包与逐文件清单位于被 Git 忽略的 `target/wan-test/ui-release/`，部署前后 API 验证摘要位于 `target/wan-test/ui-before.json` 和 `ui-after.json`。

需要回退时，将面板符号链接切回保留的旧目录，无需重启数据面服务：

```sh
ssh ubuntu@182.254.220.82 'sudo ln -s /opt/rustfrp-test/panel-before-ui-20261013-69a699c0124f /opt/rustfrp-test/panel.rollback && sudo mv -Tf /opt/rustfrp-test/panel.rollback /opt/rustfrp-test/panel'
```

## 验证结果

- 公网首页、带版本标记的引导脚本与主脚本均返回 HTTP 200；下载主脚本摘要与本地产物一致。
- 真实 Edge 浏览器恢复会话并显示新版概览、菜单与隧道管理表格，1 台在线客户端、7 条运行中隧道。
- API 检查：1 台客户端在线；内置与两个独立服务端节点全部在线；7 条隧道全部 `running`。
- 两台物理主机间 TCP `27100`、加密/压缩 TCP `27101` 各完成 8192 字节精确回显；公网 UDP `27102` 完成 128 字节精确回显。
- Controller PID `1081903`、公网独立节点 PID `1081902`、客户端 PID `2664309`、客户端主机节点 PID `2664320` 部署前后相同，服务均为 `active`。
- 原 frps PID `213397`、frpc PID `5851` 与配置摘要部署前后相同；frps 配置 SHA256 为 `049a99a74513d8c557cc3879f99d4a26ef3721ef5231392b15e9436be23e26a3`，frpc 配置 SHA256 为 `16dd50b6a26686e637c3300d2ade5a927c4f24ece420300d19cd0e265e60c1ac`。

本轮为 Web UI 静态资源发布，不是 Rust/Agent 版本升级或正式签名 Release。完整系统验收范围继续以 [双主机验收记录](WAN_ACCEPTANCE_20261013.md) 为准。
