# 覆盖率排除登记

按 `docs/DEVELOPMENT_STANDARDS.md` §1，排除项必须登记于此（文件 + 理由）。生产代码不允许就地豁免注解。

| 文件 | 理由 | 约束 |
|---|---|---|
| `rustfrp-agent/src/main.rs` | 仅做参数解析、运行时搭建、信号处理的薄入口（≤30 行），所有逻辑在库中并被测试。 | 行数 ≤ 30；不得新增逻辑 |
| `rustfrp-server/src/main.rs` | 同上。 | 行数 ≤ 30；不得新增逻辑 |
| `rustfrp-app/lib/main.dart` | Flutter 入口：初始化绑定、字体、桌面窗口、SharedPreferences 与 ProviderScope，不含逻辑；其中调用的函数各有测试（`loadWebFonts`、`RustFrpApp`）。 | ≤ 30 行 |
| `rustfrp-app/lib/core/desktop.dart` 的 `initDesktopWindow` | 需要真实的桌面窗口插件，测试宿主无法执行；其余成员（`hasCustomWindowControls` 等）有测试。 | 仅该函数；增长需复审 |

`#[cfg(test)] mod tests` 测试模块本身用 `#[cfg_attr(coverage, coverage(off))]` 排除（不计入统计），不需登记。
