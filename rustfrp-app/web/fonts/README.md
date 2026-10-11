# Web fonts

`NotoSansSC-Regular.ttf` / `NotoSansSC-Bold.ttf`: Noto Sans SC (© Google / Adobe, SIL Open Font License 1.1,
https://github.com/notofonts/noto-cjk), instanced at weight 400 / 700 and subset to GB2312 + common symbols
(≈ 8.5k characters).

The browser edition loads them at start-up (`lib/core/web/fonts.dart`): CanvasKit cannot use system fonts, and its default
fallback downloads fonts from gstatic.com, which is unreachable for many users, so Chinese text would turn into boxes.
Characters outside the subset fall back to that default.
