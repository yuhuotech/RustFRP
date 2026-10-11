# RustFRP 品牌文件

Logo 是一个**拱形隧道**，中间一个圆点表示数据穿过，左右两小段表示内外网两端——对应「内网穿透」。主色草绿 `#65A30D`（与 UI `action` token 一致）。

- `rustfrp-logo.svg`：正式矢量图标（草绿底、白色图形，外侧透明圆角）。
- `rustfrp-logo-{32,64,128,256,512,1024}.png`：PNG 图标。
- `rustfrp-mark.svg` / `-white.svg` / `-black.svg`：透明底符号（绿 / 白 / 黑），用于官网与单色场景。

使用：优先 SVG，最小 24px，四周留 1/8 空白，保持比例。
注意：`ivory` 渐变使用 `userSpaceOnUse`——对 objectBoundingBox 渐变，零高度的形状会渲染失败。
