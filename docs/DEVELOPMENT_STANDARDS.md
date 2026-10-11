# RustFRP 开发规范

> 适用于仓库内所有代码与文档；与 [技术设计文档 §0](RUSTFRP_TECH_DESIGN_V1.0.md) 的两条最高原则（极致省内存、配置完全兼容 frp）并列为强制要求。CI 不通过不得合并。

## 1. 测试覆盖率（硬性门槛）

| 范围 | 指标 | 门槛 | 工具 |
|---|---|---|---|
| 每个 Rust crate（`rustfrp-core` / `-server` / `-agent`） | **行覆盖率** | **≥ 90%** | `cargo llvm-cov` |
| 同上 | **分支覆盖率** | **≥ 90%** | `cargo +nightly llvm-cov --branch` |
| 同上 | 函数覆盖率 | ≥ 90% | 同上 |
| Flutter（`rustfrp-app`） | 行覆盖率 | ≥ 90% | `flutter test --coverage` + `tools/cov/check_lcov.py` |
| Flutter | 分支 | 见 §1.3 | — |

1. **门槛按 crate / 包整体计算，且每个 PR 都必须通过**；覆盖率只升不降（`tools/cov/baseline.json` 记录基线，下降即失败，即使仍 ≥ 90%）。
2. **新增/修改的代码行**要求 100% 被测试执行（diff coverage），避免用旧代码的高覆盖拉平新增缺口。
3. **禁止刷覆盖率**：没有断言的测试、只调用不校验结果的测试，评审一律打回。覆盖率是底线，不是目标；每个分支都要有**断言其行为**的用例（含错误分支、边界、空值、超限）。
4. **排除项**必须登记在 `tools/cov/exclusions.md`（文件/函数 + 理由 + 责任人），仅允许：
   - `main.rs` 中仅做参数解析与转调的薄入口（逻辑必须在库里，入口 ≤ 30 行）；
   - 平台专属且无法在 CI 平台执行的系统调用薄封装（如 Windows 服务注册），需有对应的手工验证清单；
   - 自动生成的代码。
   其他一律不得排除。**生产代码**禁止任何就地豁免注解（`coverage(off)`、`tarpaulin_include` 等）。
   唯一例外：`#[cfg(test)] mod tests` 测试模块本身用 `#[cfg_attr(coverage, coverage(off))]` 排除，避免测试代码的断言宏分支稀释统计。
5. 无法用单元测试触达的代码（网络、时钟、文件系统）要**通过 trait 注入假实现**来测，而不是排除。设计上优先让逻辑可测。

### 1.1 Rust 的运行方式

```sh
make core-cov        # 单个 crate：行/函数/分支，低于 90% 失败
make cov             # 全仓库汇总 + HTML 报告（target/llvm-cov/html）
```

分支覆盖使用 `-Z coverage-options=branch`（`cargo llvm-cov --branch`）。在 stable 上通过 `RUSTC_BOOTSTRAP=1` 启用，**仅用于覆盖率任务，产物构建不设置它**。
`cargo-llvm-cov` 自身不提供「分支覆盖率门槛」参数，因此统一由 `tools/cov/check_lcov.py` 读取 lcov 的 BRF/BRH 校验（并比对 `tools/cov/baseline.json`，只升不降）。

### 1.2 测试分层与要求

| 层 | 位置 | 要求 |
|---|---|---|
| 单元测试 | 与源码同文件的 `#[cfg(test)] mod tests` | 每个公开函数、每个分支；错误路径必测 |
| 属性/模糊测试 | `proptest`（编解码、解析器、状态机、对账引擎） | 往返不变量：`decode(encode(x)) == x`；解析器对任意输入**不 panic** |
| 集成测试 | 各 crate `tests/` | 真实 SQLite、真实回环 TCP/WS、假 Agent；覆盖 spec 里的验收用例（AT-xx） |
| 兼容性测试 | `rustfrp-protocol/frp-compat/` | frp 官方样例与边界配置；解析结果、默认值、错误信息与 frp 一致；**每新增一个兼容字段先加用例** |
| 协议一致性 | `rustfrp-protocol/fixtures/` | Rust 与 Dart 读同一批 JSON，往返相等 |
| 端到端 | `tools/e2e/` | Controller + 多 Agent 全链路，含热更新不断连、断线恢复 |
| 基准 | `tools/bench/` | 内存（RSS）与吞吐；超基线 10% 失败（原则一） |
| UI | `flutter test` | widget、golden（宽/窄 × 浅/深）、store 单测 |

### 1.3 Flutter 的分支覆盖说明

Dart 官方 `--coverage` 只产出**行覆盖**（lcov 无 BRDA 分支数据），目前没有成熟的 Dart 分支覆盖工具。因此：

- 门槛：**行覆盖 ≥ 90%** 由 CI 强制；
- 分支纪律：每个 `if/else`、`switch`、三元、`?.`/`??` 的各个走向必须有用例，**由评审清单检查**（PR 模板里有勾选项）；
- 业务逻辑尽量放在可测的纯 Dart 类（store / view model），UI 层保持薄；
- 若后续出现可靠的 Dart 分支覆盖工具，再把门槛升级为强制。

## 2. 开发流程

1. **先协议、后 Rust、再 Flutter**；改 Model 必须同步 `rustfrp-protocol/` 与 fixtures。
2. **测试先行**：新功能先写失败的测试（至少是验收用例的骨架），再实现。
3. **小步提交**：一个提交一件事；提交信息 `type(scope): 说明`（`feat/fix/refactor/test/docs/chore`，scope 如 `core`、`server`、`agent`、`app`、`web`）。
4. **主干始终可运行**；每阶段结束打 tag。
5. **PR 必须附**：变更说明、测试说明、覆盖率变化、内存基准变化（涉及运行时的改动）。

## 3. Rust 代码规范

- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings` 必须通过；workspace 级 `[lints]` 统一配置。
- **禁止** 在非测试代码中使用 `unwrap()` / `expect()` / `panic!` / 索引越界风险写法（clippy `unwrap_used`、`expect_used`、`indexing_slicing` 设为 deny）；错误用 `thiserror` 定义的类型向上传递。例外：不变量断言需写明理由并经评审。
- `unsafe` 默认禁止（`#![forbid(unsafe_code)]`）；确需使用（如 `splice`）集中在 `rustfrp-core::sys`，每处写 `// SAFETY:` 说明并有专门测试。
- **内存纪律（原则一）**：不得随意 `clone()` 大对象、`collect()` 大集合、`String` 拼接大文本；容器必须有容量上限；不使用无界 channel；新增依赖需在 PR 里说明体积与堆分配影响，并通过 `cargo deny`。
- **配置兼容纪律（原则二）**：frp 字段名、默认值、校验与 frp 一致；不支持的字段必须进入兼容报告，不得静默忽略。
- 异步：不得在 async 中做阻塞调用（文件/SQLite 用专用线程）；所有网络操作有超时；所有后台任务可取消。
- 日志用 `tracing`；**不得**记录令牌、密钥、密码、流量内容。
- 公开 API 写文档注释；注释只解释「为什么」，不复述代码。

## 4. Flutter / Dart 规范

- 颜色/字号/圆角只用 token；控件只用 `shared/widgets.dart`（见 UI_SPEC）；新页面用 `PageHeader`。
- `flutter analyze` 零警告；`flutter test` 全绿；golden 在宽/窄 × 浅/深下更新。
- Model 不可变（`@immutable`、`copyWith`、`==`）；业务逻辑放 store/纯类，Widget 保持薄。
- 平台差异只放 `core/platform/` 的条件导入内。

## 5. 安全与依赖

- 秘密永不入库入日志入测试快照；测试用固定的假密钥。
- `cargo deny check`（许可证、已知漏洞、重复依赖）与 `cargo audit` 进 CI。
- 第三方依赖要锁版本（`Cargo.lock` 入库）；升级单独提交。

## 6. 文档

- 行为/协议/UI 变更同步文档（`docs/`、`PROTOCOL.md`、`UI_SPEC.md`），**文档与代码冲突视为缺陷**。
- 排除项、基线、已知限制各有登记文件，不散落在代码注释里。

## 7. 合并检查清单（PR 模板）

- [ ] fmt / clippy / analyze 无警告
- [ ] 测试全绿；行、分支（Rust）、函数覆盖率均 ≥ 90%，且未低于基线
- [ ] 新增/修改代码行 100% 被测试覆盖，且测试带有效断言
- [ ] 错误分支、边界、超限都有用例
- [ ] 协议/fixtures/文档已同步
- [ ] 内存基准未回归（运行时改动）
- [ ] 无新增未登记的覆盖率排除项
- [ ] 无秘密泄露；日志无敏感信息
