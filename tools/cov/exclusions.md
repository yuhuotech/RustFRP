# 覆盖率排除登记

按 `docs/DEVELOPMENT_STANDARDS.md` §1，排除项必须登记于此（文件 + 理由）。生产代码不允许就地豁免注解。

| 文件 | 理由 | 约束 |
|---|---|---|
| `rustfrp-agent/src/main.rs` | 仅做参数解析、运行时搭建、信号处理的薄入口（≤30 行），所有逻辑在库中并被测试。 | 行数 ≤ 30；不得新增逻辑 |
| `rustfrp-server/src/main.rs` | 同上。 | 行数 ≤ 30；不得新增逻辑 |
| `rustfrp-desktop/src/main.rs` | 仅把进程入口转调到已验收的库函数。 | ≤ 30 行；不得新增逻辑 |
| `rustfrp-desktop/src/native_calls.rs` | Tauri IPC 属性/事件回调的薄适配；负责请求来源授权后转调桌面 support 业务层，以及调用系统剪贴板、默认浏览器、托盘、通知、文件选择器、窗口和退出 API。所有请求/动作校验与业务结果在 `rustfrp-desktop-support` 中按覆盖率验收；真实 macOS WebView E2E 验收 IPC 与系统集成。 | 仅宿主适配层；权限、边界和业务逻辑不得迁入此文件；保留平台手工验收清单 |
| `rustfrp-desktop/src/native_e2e.rs` | 只在 `native-e2e` feature 下编译的测试驱动命令，用于注入临时凭据、记录阶段和驱动真实窗口 E2E，不进入正常桌面包。 | 必须保持 `native-e2e` feature 隔离；每条命令须由真实系统 WebView E2E 调用 |

`#[cfg(test)] mod tests` 测试模块本身用 `#[cfg_attr(coverage, coverage(off))]` 排除（不计入统计），不需登记。
