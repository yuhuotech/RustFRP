# 原生 UI 重构实施与验收

更新于 2026-10-15。目标按 [重构计划](UI_REFACTOR_PLAN_20261014.md) 执行：Web 与桌面共用原生 HTML/CSS/TypeScript 页面，迁移原有功能并删除 Flutter 工程、依赖和活动构建入口。覆盖率和自动化验收已达到本地门槛；本机外平台、真实签名 Release、桌面进程树内存对照及一个窗口恢复性能目标仍按实测边界记录。

基线：`6058ade`；重构分支：`refactor/native-ui`。重构批次已提交，没有推送。

## 阶段结果

| 阶段 | 状态 | 证据与边界 |
|---|---|---|
| P0 功能盘点与基线 | 部分完成 | 迁移矩阵、旧 Web/桌面产物和 Web 性能基线已留存；Flutter 桌面与系统空白 WebView 的进程树内存对照未取得 |
| P1 双端架构与资源 | 本机验收通过 | 同一资源用于 Controller 静态托管和真实 macOS WKWebView；会话、桥接权限、资源摘要与 CSP 有浏览器/桌面证据 |
| P2 交互底座 | 本机验收通过 | 深浅色、宽窄屏、真实系统字体、焦点、键盘事件、轮询时弹窗、缓存升级和资源预算通过 |
| P3 管理页面迁移 | 自动化验收通过 | 真实 Controller/Agent 覆盖节点、客户端、访问者、全部支持的隧道类型、权限、导入导出和审计 |
| P4 桌面能力迁移 | macOS 自动化验收通过 | 受管/独立模式、配置、诊断、Agent 生命周期、窗口恢复、单实例和隔离启动已测；其他桌面系统及真实签名升级未实机验收 |
| P5 全量质量验证 | 本地质量门槛通过 | `make lint test cov`、三引擎浏览器 E2E、macOS 系统 WebView、全栈 E2E、内存和安装器验证通过；窗口首帧 p95 超过建议值 |
| P6 切换与清理 | 代码清理完成 | `rustfrp-app/`、Flutter 依赖、命令和活动入口已删除；旧偏好与 Service Worker 清理仍作为数据/缓存迁移兼容代码 |

## 功能迁移证据

- 登录、会话恢复/失效、服务器切换、角色、CSRF、同源限制和审计理由：Playwright 使用真实 Controller 验证成功、拒绝与审计结果。
- 概览六卡、24h/7d 流量、筛选、采样缺口、节点详情和独立节点接入/删除：共享 UI E2E 覆盖真实 Controller 数据。
- 客户端标签、连接上限、接入/下线/重启/升级/迁移，以及访问者 CRUD、权限、加密和压缩：真实 API/Agent E2E 与 Rust 测试通过。
- TCP、UDP、HTTP、HTTPS、TCPMUX、STCP、SUDP 与插件选项、模板、批量操作：完整隧道编辑器 E2E 通过；转发路径由真实 Agent/回环后端验证。
- 配置导入预览、兼容警告、摘要确认、应用、导出；审计分页、Controller/Agent 日志、gap、刷新和失败反馈：行为测试及真实 API E2E 通过。
- 设置账号、主题、管理员保护、接入令牌/API Token 与一次性凭据：真实角色、审计、撤销和敏感信息保护用例通过。
- 桌面受管和独立模式、随包 Agent、退出后存活、后台恢复、完整 TOML/YAML/JSON 编辑、verify、私有暂存/原子保存/回滚、状态/日志/流量/脱敏诊断：支持库单测和 macOS 系统 WebView/本机控制 E2E 通过。
- 单实例、托盘策略、关闭到托盘、自启动和窗口销毁恢复：macOS 桌面 E2E 覆盖；Linux/Windows 托盘与系统通知权限未在本机实测。
- Agent/App 更新的签名、摘要、版本、取消、失败和回滚代码路径由支持库测试覆盖；由于没有发布签名密钥，本期没有真实签名包升级证据。

## 最终门槛

- [x] Rust crate、桌面两个 crate、共享 UI、静态资源 crate 的行/函数/分支覆盖率均 ≥90%。
- [x] `make lint test cov` 通过；桌面和共享 UI 的 fmt/clippy、类型检查及单测通过。
- [x] Playwright Chromium、Firefox、WebKit 三引擎共 30 项通过；真实 Controller 写操作、权限、审计和缓存迁移均有断言。
- [x] 1440/1024/720/390/300px、深浅色、系统字体、键盘焦点和合成输入事件截图/用例通过。合成事件不等同于真实操作系统 IME 实机验收。
- [x] macOS WKWebView 登录、一次性凭据保护、30 次窗口循环、单实例、释放/恢复、后台 Agent 存活及退出后 TCP 转发通过。
- [x] `make e2e`、`make bench-mem`、`make installer-test` 通过；共享 UI gzip 预算通过。
- [x] CSP、资源摘要、静态目录/内嵌资源、缓存清理、路径与 IPC 权限边界通过。
- [x] 默认构建/启动/打包、CI、覆盖率命令已切换到共享 UI 与 Tauri；本地 macOS `.app` 构建和 ad-hoc 签名校验通过。
- [x] Flutter 工程、依赖、测试/覆盖率入口已删除；活跃 Makefile/CI/Release 流程无 Flutter 工具链依赖。
- [x] 本文与 README、交接文档记录了当前证据、限制和回退入口。
- [x] 按用户要求，本期不设置也不执行 24 小时稳定性测试。

## 覆盖率与资源数据

最近一次 `make cov` 的 lcov 门槛结果：

| Crate / 包 | 行 | 函数 | 分支 |
|---|---:|---:|---:|
| `rustfrp-core` | 99.03% | 97.97% | 97.15% |
| `rustfrp-agent` | 97.08% | 91.41% | 91.43% |
| `rustfrp-server` | 96.84% | 95.72% | 91.30% |
| `rustfrp-ui-assets` | 97.37% | 100.00% | 90.00% |
| `rustfrp-desktop` | 98.93% | 100.00% | 100.00% |
| `rustfrp-desktop-support` | 95.81% | 93.23% | 90.34% |
| `rustfrp-ui` | 99.30% | 93.53% | 90.16% |

所有比例来自 `make cov` 生成的 lcov 报告；每个现行包都通过，没有为本次重构降低门槛。

- UI 浏览器短时样本：50 客户端/500 隧道/7 天流量数据下，Chromium 冷登录外壳 p95 29.4ms，本地反馈 p95 26ms，GC 后 JS heap 4,488,524 字节；浏览器场景报告为 `target/ui-performance/smoke.json`。
- 静态资源：当前活动发布 gzip 合计 50,822 字节，首屏 15,519 字节；完整分发包含保留哈希资源为 95,117 字节，均在预算内。
- Controller：同一 50/500 压力样本的 RSS 为 28,508,160 字节。该数据不等价于空闲典型场景 ≤12 MiB，也没有单独量出静态托管稳态增量。
- Agent/Edge：10 隧道/100 连接的本机基准峰值 Client 7.14 MiB、Edge 6.70 MiB，低于 10 MiB。
- macOS `.app` 构建包约 11.06 MiB；旧 Flutter macOS 应用包约 50 MiB。它们是安装包体积，不是桌面进程内存对照。
- macOS release 优化构建的真实系统 WebView 完成 30 次窗口隐藏/恢复；从恢复操作到 WebView 可见并完成两帧渲染的 p95 为 516ms。计划建议值是 200ms，这项尚未达标；数据在 `target/ui/native-window-e2e.json`。窗口交互以外的本地反馈 p95 26ms 已通过浏览器测量。
- 桌面 WebView、Tauri 宿主、辅助进程和 Agent 的同场景 RSS/PSS/物理占用比较尚未完成；不能据包体积推断内存下降比例。

## Flutter 清理边界与仍开放的外部验收

`rustfrp-app/` 源码目录及 Flutter 构建入口已从 Git 工作树删除。仓库只保留两类兼容代码：`rustfrp-desktop-support` 读取旧 `flutter.*` 偏好并迁移用户设置；Web 兼容 Service Worker 清除旧 Flutter 缓存并注销旧注册。这些代码不加载或运行 Flutter renderer。旧 macOS/Web 构建基线保存在被 Git 忽略的 `target/ui-refactor-baseline/`，只用于历史对照。

本轮可以在当前 macOS 主机证明的本地功能、测试和覆盖率已通过。Linux WebKitGTK 补测尝试在 Colima Ubuntu 24.04 容器中启动，但 apt 在安装前报告虚拟磁盘无剩余空间；Colima 配置盘为 80 GiB，容器运行时还有其他服务和卷，因此没有清理或重建它。Linux/Windows 真实系统 WebView、跨平台安装/系统通知实机验收、隔离保存的旧 Flutter 基线与当前桌面包的进程树内存对照，以及真实签名 Release/升级仍须在具备对应系统、存储或凭据时完成；不把本地测试冒充这些外部证据。
