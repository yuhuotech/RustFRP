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
make core-cov    # rustfrp-core 覆盖率（行/函数/分支）并校验门槛与基线
```
Flutter 部分（`rustfrp-app`）尚未创建，规范见设计文档 §2–§3、§5.6。

## 仓库结构（现状）
- `rustfrp-core/`：Rust 共享库。已实现 `frpcfg`（frp 配置兼容层：解析 / 默认值 / 校验 / 兼容报告 / 导出）。
- `rustfrp-protocol/frp-compat/`：兼容性测试语料（含上游 frp 官方示例）。
- `assets/brand/`：Logo。
- `docs/`：需求、技术设计、开发规范。
- 待建：`rustfrp-server/`、`rustfrp-agent/`、`rustfrp-app/`、`rustfrp-web/`（见设计文档 §5）。

## 当前进度
- P00 工程初始化：Cargo workspace、CI、覆盖率门槛工具 ✔；Flutter 工程 ✘。
- P01：`frpcfg` ✔（INI 转换 `convert` 未做）；传输 / 多路复用 / 代理运行时 ✘。
