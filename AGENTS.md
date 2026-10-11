# AGENTS.md — RustFRP 协作约定

> 给人和 AI 编程助手共同使用。**开始任何工作前先读：** `docs/RUSTFRP_TECH_DESIGN_V1.0.md` §0（两条最高原则）与 `docs/DEVELOPMENT_STANDARDS.md`。

## 两条最高原则
1. **极致省内存**：典型场景 Agent / Edge 常驻 ≈10 MB 以内。新增功能须评估增量内存，避免无界容器、整包缓冲、per-connection 线程。
2. **配置层完全兼容 frp**：`frps.toml` / `frpc.toml` 无需修改即可使用；不支持的字段必须进入兼容报告，**不得静默忽略**。frp 官方源码是行为的最终依据（语料见 `rustfrp-protocol/frp-compat/upstream`）。

## 强制门槛
- 每个 Rust crate：**行 / 函数 / 分支覆盖率均 ≥ 90%**，且不得低于 `tools/cov/baseline.json`；新增代码要有有效断言的测试。
- `cargo fmt --check`、`cargo clippy --all-targets -D warnings`；非测试代码禁止 `unwrap/expect/panic/索引`，`unsafe` 默认 forbid。

## 常用命令
```sh
make lint        # fmt --check + clippy
make test        # cargo test --workspace
make cov         # 全部 crate 覆盖率（行/函数/分支）并校验门槛与基线
make bench-mem   # release 内存基准（≤10 MB）
```
Flutter 部分（`rustfrp-app`）尚未创建，规范见设计文档 §2–§3、§5.6。

## 仓库结构（现状）
- `rustfrp-core/`：Rust 共享库。已实现 `frpcfg`（frp 配置兼容层：解析 / 默认值 / 校验 / 兼容报告 / 导出）。
- `rustfrp-protocol/frp-compat/`：兼容性测试语料（含上游 frp 官方示例）。
- `assets/brand/`：Logo。
- `docs/`：需求、技术设计、开发规范。
- 待建：`rustfrp-server/`、`rustfrp-agent/`、`rustfrp-app/`、`rustfrp-web/`（见设计文档 §5）。

## 架构要点（Controller）
- `rustfrp-server` = SQLite（单连接 + spawn_blocking）+ axum 管理 API + **内置 Edge**（`rustfrp-agent::edge`，通过 `Policy` 接入 `DbPolicy`）。
- 设备接入：Client 用一次性令牌换身份（Ed25519 公钥入库）；之后用签名登录。**配置下发复用隧道连接**（控制流上的 `ServerMsg::Config` 全量快照，Edge 对账增删代理，无 WebSocket）。
- 代理名即隧道 ID；隧道状态由 Edge 回报（`ProxyEvent`）写回 DB。
- 管理 API：Cookie 会话 + CSRF + 同源检查 + 登录限流；写操作必须带 `reason` 并与业务同事务写审计。管理员只能经 CLI（`rustfrp-server admin create`）创建。

## 当前进度
- P00 工程初始化：Cargo workspace、CI、覆盖率门槛工具 ✔；Flutter 工程 ✘。
- P01 隧道核心：`frpcfg` ✔（INI 转换 `convert` 未做）；TLS1.3 + yamux 多路复用 + TCP 隧道 + 令牌鉴权 + 重连 + 心跳 ✔；
  `rustfrp-agent` 的 client / edge 模式、`verify`、frpc/frps 别名 ✔；内存基线 ✔（见 `tools/bench/README.md`）。
  **尚未实现**：UDP、http/https/vhost、stcp/sudp/xtcp、tcpmux、插件、带宽限制、健康检查、负载均衡、visitors、PROXY protocol——
  配置里出现这些时会在兼容报告 / 日志里明确列出并跳过。
- P02 Controller：`rustfrp-server`（SQLite、管理员 CLI、接入令牌、设备管理、隧道 CRUD、配置热下发、审计、REST、面板静态托管）✔；
  `rustfrp-agent --controller … --enroll-token …` 受管模式 ✔。尚未做：流量统计/曲线、日志尾随、API Token、多 Edge 节点、安装脚本 `install.sh`/`install.ps1` 的托管。
