# RustFRP 品牌文件

Logo 使用**一笔贯通的倾斜双环**：两个闭合环分别代表内网与外网，中心收束处连为一体，表达经隧道持续互通。用流畅曲线和两个大面积负形取代具象的门洞、箭头及孤立节点；主体采用已确认的放大版（较初版放大 12%），并保持均衡留白。双环是连接的视觉隐喻，不表示无限带宽。

保留草绿渐变 `#84CC16 → #65A30D → #4D7C0F`，主色与 UI action token 一致；主体为白色至浅象牙色渐变。

- `rustfrp-logo.svg`：唯一设计源，草绿底、白色符号、透明圆角。
- `rustfrp-logo-{16,32,64,128,256,512,1024}.png`：各尺寸图标。
- `rustfrp-mark.svg` / `-white.svg` / `-black.svg`：同一几何的绿、白、黑透明底符号。
- `rustfrp-logo-larger-512.png` / `rustfrp-logo-larger.svg`：已确认的放大版样稿，与正式版一致。
- `rustfrp-logo-size-comparison.png`：历史对比，左为原尺寸，右为现行放大版。
- `preview.html`：浅色、深色与 16–128px 的实际尺寸预览。

## 重新生成

安装 `cairosvg` 和 `pillow` 后，从仓库根目录运行：

```sh
python3 tools/gen_icons.py
```

同步更新品牌 PNG、Tauri 桌面图标、Web favicon 与 PWA 图标。PWA maskable 版为不透明方形底，主体缩至安全圆内。脚本不改业务代码，不增加 Agent / Edge 运行时内存。

当前工程没有 Android / iOS 工程、Linux 图标安装配置或托盘图标；后续新增这些入口时应从本设计源生成，勿恢复旧图形。

优先用 SVG；图标最低 16px（有条件时至少 24px）。保持比例，不再叠加额外内边距；主图标已含安全留白。
