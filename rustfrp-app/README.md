# RustFRP Flutter 客户端

本目录包含 Flutter Web 管理面板及 macOS、Windows、Linux 桌面客户端。桌面运行支持管理员控制台、本机受管 Agent 和读取本地 TOML 的独立模式。

产品范围、逐项完成情况和未完成需求以仓库根目录 [`README.md`](../README.md) 与 [`docs/IMPLEMENTATION_STATUS.md`](../docs/IMPLEMENTATION_STATUS.md) 为准。桌面平台自动化测试可通过根目录 `make app-test` 执行；Web 发布构建使用 `make web-build`（禁用外部 CDN）。

桌面发布包依赖 Release 工作流随包提供的 `rustfrp-agent`。构建、签名、公证和发布凭据说明见 [`tools/release/README.md`](../tools/release/README.md)；真实三平台安装验收尚未完成。

## 本地查看管理面板

推荐在仓库根目录运行 `./start_dev.sh`：自动构建面板与 Controller，在后台启动 <http://localhost:9674>。Web 端口固定为 `9674`，启动时会自动回收旧的 RustFRP Controller 并重启。首次空数据库会要求设置 `preview` 管理员密码；已有管理员时保留账号与密码。`./start_dev.sh --skip-build` 使用现有构建快速启动，`./start_dev.sh status` 查看运行状态，`./start_dev.sh stop` 停止服务。日志在 `target/dev-panel/server.log`。修改 UI 后重新运行脚本，浏览器强制刷新。

可用 `FLUTTER=/完整路径/flutter` 指定 Flutter，`DEV_EDGE_PORT` 调整本地 Edge 端口，`DEV_DB` 指定另一份本地数据库；Web 管理面板始终使用 `9674`。脚本默认复用 `target/panel-preview.db`。

手动启动步骤：

在仓库根目录执行以下命令。使用独立的本地预览数据库和 `9674` 端口；内置 Edge 使用本机 `17000` 端口，避免与 macOS 的 `7000` 端口冲突。

```sh
make web-build
cargo build --release -p rustfrp-server
# 首次运行创建本地管理员，命令等待时输入密码并回车
RUSTFRP_DB=target/panel-preview.db target/release/rustfrp-server admin create preview
RUSTFRP_DB=target/panel-preview.db RUSTFRP_HTTP=127.0.0.1:9674 RUSTFRP_EDGE=127.0.0.1:17000 RUSTFRP_PUBLIC_HOST=127.0.0.1 RUSTFRP_PANEL_DIR="$PWD/rustfrp-app/build/web" target/release/rustfrp-server run
```

打开 <http://127.0.0.1:9674>，使用 `preview` 和刚设置的密码登录。此数据库首次打开没有客户端和隧道，页面显示真实的本地 Controller 数据。后续修改 UI 后执行 `make web-build`，浏览器强制刷新即可，Controller 无需重启。运行终端按 `Ctrl+C` 停止服务。

概览指标区在内容宽度 ≥1040px 时六卡一行，≥560px 时三卡一行，≥300px 时两卡一行，更窄时单列；卡片使用紧凑内边距，长指标和说明可悬停查看完整内容。
