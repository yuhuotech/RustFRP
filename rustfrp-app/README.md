# RustFRP Flutter 客户端

本目录包含 Flutter Web 管理面板及 macOS、Windows、Linux 桌面客户端。桌面运行支持管理员控制台、本机受管 Agent 和读取本地 TOML 的独立模式。

产品范围、逐项完成情况和未完成需求以仓库根目录 [`README.md`](../README.md) 与 [`docs/IMPLEMENTATION_STATUS.md`](../docs/IMPLEMENTATION_STATUS.md) 为准。桌面平台自动化测试可通过根目录 `make app-test` 执行；Web 发布构建使用 `make web-build`（禁用外部 CDN）。

桌面发布包依赖 Release 工作流随包提供的 `rustfrp-agent`。构建、签名、公证和发布凭据说明见 [`tools/release/README.md`](../tools/release/README.md)；真实三平台安装验收尚未完成。
