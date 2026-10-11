import 'package:flutter/material.dart';

/// Design tokens ported 1:1 from Linkory (itself ported from cc-switch v7).
/// The ONLY change: the `action` family is grass green instead of orange.
/// Everything else is intentionally identical — see docs/RUSTFRP_TECH_DESIGN_V1.0.md §2.2.
@immutable
class RustFrpColors extends ThemeExtension<RustFrpColors> {
  const RustFrpColors({
    required this.bgApp,
    required this.bgSidebar,
    required this.bgSubtle,
    required this.bgSelected,
    required this.bgCard,
    required this.border,
    required this.borderStrong,
    required this.text1,
    required this.text2,
    required this.text3,
    required this.action,
    required this.actionFg,
    required this.actionHover,
    required this.actionText,
    required this.actionSoft,
    required this.controlOff,
    required this.direct,
    required this.directText,
    required this.directSoft,
    required this.success,
    required this.successText,
    required this.successSoft,
    required this.warning,
    required this.warningText,
    required this.warningSoft,
    required this.danger,
    required this.dangerText,
    required this.dangerSoft,
    required this.overlay,
    required this.shadowSm,
    required this.shadowMd,
    required this.shadowLg,
  });

  final Color bgApp, bgSidebar, bgSubtle, bgSelected, bgCard;
  final Color border, borderStrong;
  final Color text1, text2, text3;
  final Color action, actionFg, actionHover, actionText, actionSoft;
  final Color controlOff;
  final Color direct, directText, directSoft;
  final Color success, successText, successSoft;
  final Color warning, warningText, warningSoft;
  final Color danger, dangerText, dangerSoft;
  final Color overlay;
  final List<BoxShadow> shadowSm, shadowMd, shadowLg;

  static const light = RustFrpColors(
    bgApp: Color(0xFFFFFFFF),
    bgSidebar: Color(0xFFF5F5F7),
    bgSubtle: Color(0xFFEFEFF2),
    bgSelected: Color(0xFFE4E4EA),
    bgCard: Color(0xFFFFFFFF),
    border: Color(0xFFE4E4E8), // hsl(240 10.2% 90.4%)
    borderStrong: Color(0xFFCFCFD6),
    text1: Color(0xFF18181B),
    text2: Color(0xFF52525B),
    text3: Color(0xFF65656E),
    action: Color(0xFF65A30D),
    actionFg: Color(0xFFFFFFFF),
    actionHover: Color(0xFF4D7C0F),
    actionText: Color(0xFF3F6212),
    actionSoft: Color(0xFFF0F9E1),
    controlOff: Color(0xFF888890),
    direct: Color(0xFF0A84FF),
    directText: Color(0xFF0063C7),
    directSoft: Color(0xFFEAF3FF),
    success: Color(0xFF16A34A),
    successText: Color(0xFF007634),
    successSoft: Color(0xFFE8F7EE),
    warning: Color(0xFFD97706),
    warningText: Color(0xFFA64A00),
    warningSoft: Color(0xFFFEF3C7),
    danger: Color(0xFFDC2626),
    dangerText: Color(0xFFB91C1C),
    dangerSoft: Color(0xFFFEE2E2),
    overlay: Color(0x5C09090B),
    shadowSm: [
      BoxShadow(color: Color(0x0F101828), blurRadius: 2, offset: Offset(0, 1)),
    ],
    shadowMd: [
      BoxShadow(color: Color(0x14101828), blurRadius: 12, offset: Offset(0, 4)),
      BoxShadow(color: Color(0x0F101828), blurRadius: 3, offset: Offset(0, 1)),
    ],
    shadowLg: [
      BoxShadow(
        color: Color(0x29101828),
        blurRadius: 40,
        offset: Offset(0, 20),
      ),
      BoxShadow(color: Color(0x14101828), blurRadius: 12, offset: Offset(0, 4)),
    ],
  );

  static const dark = RustFrpColors(
    bgApp: Color(0xFF1C1C1E),
    bgSidebar: Color(0xFF161618),
    bgSubtle: Color(0xFF26262A),
    bgSelected: Color(0xFF313136),
    bgCard: Color(0xFF2E2E33),
    border: Color(0xFF3E3E45), // hsl(240 5.3% 25.7%)
    borderStrong: Color(0xFF4A4A52),
    text1: Color(0xFFF4F4F5),
    text2: Color(0xFFD0D0D8),
    text3: Color(0xFFB5B5BD),
    action: Color(0xFF4D7C0F),
    actionFg: Color(0xFFFFFFFF),
    actionHover: Color(0xFF65A30D),
    actionText: Color(0xFFA3E635),
    actionSoft: Color(0x1F65A30D),
    controlOff: Color(0xFF6F6F77),
    direct: Color(0xFF4A8FD9),
    directText: Color(0xFF8FB8E6),
    directSoft: Color(0x1F4A8FD9),
    success: Color(0xFF4ADE80),
    successText: Color(0xFF4ADE80),
    successSoft: Color(0xFF0F2B1A),
    warning: Color(0xFFFBBF24),
    warningText: Color(0xFFFCD34D),
    warningSoft: Color(0xFF33260A),
    danger: Color(0xFFF87171),
    dangerText: Color(0xFFFCA5A5),
    dangerSoft: Color(0xFF3A1414),
    overlay: Color(0x8C000000),
    shadowSm: [
      BoxShadow(color: Color(0x66000000), blurRadius: 2, offset: Offset(0, 1)),
    ],
    shadowMd: [
      BoxShadow(color: Color(0x73000000), blurRadius: 12, offset: Offset(0, 4)),
      BoxShadow(color: Color(0x66000000), blurRadius: 3, offset: Offset(0, 1)),
    ],
    shadowLg: [
      BoxShadow(
        color: Color(0x80000000),
        blurRadius: 40,
        offset: Offset(0, 20),
      ),
      BoxShadow(color: Color(0x66000000), blurRadius: 12, offset: Offset(0, 4)),
    ],
  );

  @override
  RustFrpColors copyWith() => this;

  @override
  RustFrpColors lerp(ThemeExtension<RustFrpColors>? other, double t) =>
      t < 0.5 ? this : (other as RustFrpColors? ?? this);
}

/// Radii (tailwind: control 6, panel 10, dialog 14).
class Radii {
  static const control = 6.0;
  static const panel = 10.0;
  static const dialog = 14.0;
}

/// Type scale (tailwind fontSize): size / line-height / weight.
class Type {
  static const fontFamilyFallback = [
    'NotoSansSC', // web only: bundled, see web/fonts/README.md
    '.AppleSystemUIFont',
    'PingFang SC',
    'Hiragino Sans GB',
    'Segoe UI Variable Text',
    'Segoe UI',
    'Microsoft YaHei UI',
    'Microsoft YaHei',
    'Roboto',
    'Helvetica Neue',
    'Arial',
    'sans-serif',
  ];
  static const monoFallback = [
    'SF Mono', 'Menlo', 'Consolas', 'Liberation Mono', 'monospace',
    ...fontFamilyFallback, // CJK glyphs, including the bundled Web font.
  ];

  static TextStyle _s(double size, double lh, FontWeight w) => TextStyle(
    fontSize: size,
    height: lh / size,
    fontWeight: w,
    fontFamilyFallback: fontFamilyFallback,
  );

  static final badge = _s(11, 16, FontWeight.w500);
  static final caption = _s(12, 18, FontWeight.w400);
  static final body = _s(13, 20, FontWeight.w400);
  static final strong = _s(14, 20, FontWeight.w500);
  static final section = _s(15, 22, FontWeight.w600);
  static final title = _s(16, 24, FontWeight.w600);
  static final page = _s(18, 26, FontWeight.w600);
  static final metric = _s(24, 32, FontWeight.w600);
}

extension RustFrpTheme on BuildContext {
  RustFrpColors get c => Theme.of(this).extension<RustFrpColors>()!;
}

ThemeData buildTheme(Brightness b) {
  final c = b == Brightness.light ? RustFrpColors.light : RustFrpColors.dark;
  return ThemeData(
    brightness: b,
    useMaterial3: true,
    scaffoldBackgroundColor: c.bgApp,
    canvasColor: c.bgApp,
    dividerColor: c.border,
    splashFactory: NoSplash.splashFactory,
    highlightColor: Colors.transparent,
    hoverColor: Colors.transparent,
    colorScheme: ColorScheme.fromSeed(seedColor: c.action, brightness: b)
        .copyWith(
          primary: c.action,
          onPrimary: c.actionFg,
          surface: c.bgApp,
          onSurface: c.text1,
          outline: c.border,
          error: c.danger,
        ),
    textTheme: TextTheme(bodyMedium: Type.body.copyWith(color: c.text1)),
    extensions: [c],
  );
}
