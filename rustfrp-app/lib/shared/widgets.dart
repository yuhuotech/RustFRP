import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:window_manager/window_manager.dart';

import '../core/desktop.dart';

import '../theme/tokens.dart';

/// Unified brand asset shared by login and the desktop rail.
class BrandLogo extends StatelessWidget {
  const BrandLogo({super.key, this.size = 28});
  final double size;

  @override
  Widget build(BuildContext context) => Image.asset(
        'assets/brand/rustfrp-logo-128.png',
        width: size,
        height: size,
        filterQuality: FilterQuality.high,
        semanticLabel: 'RustFRP',
      );
}

/// Fixed home entry in the desktop rail.
class BrandHomeButton extends StatefulWidget {
  const BrandHomeButton({super.key, required this.onPressed});
  final VoidCallback onPressed;
  @override
  State<BrandHomeButton> createState() => _BrandHomeButtonState();
}

class _BrandHomeButtonState extends State<BrandHomeButton> {
  bool _down = false;
  @override
  Widget build(BuildContext context) => Tooltip(
    message: '返回首页',
    child: MouseRegion(
      cursor: SystemMouseCursors.click,
      child: Semantics(
        button: true,
        label: '返回首页',
        child: GestureDetector(
          onTap: widget.onPressed,
          onTapDown: (_) => setState(() => _down = true),
          onTapUp: (_) => setState(() => _down = false),
          onTapCancel: () => setState(() => _down = false),
          child: AnimatedScale(
            scale: _down ? .96 : 1,
            duration: const Duration(milliseconds: 100),
            child: const BrandLogo(),
          ),
        ),
      ),
    ),
  );
}

enum BtnVariant { solid, neutral, quiet, ghost, destructive }

/// cc-switch Button: compact 28 / regular 32, radius 6, press scales to .96.
class LButton extends StatefulWidget {
  const LButton({
    super.key,
    required this.label,
    this.icon,
    this.onPressed,
    this.variant = BtnVariant.neutral,
    this.compact = false,
    this.loading = false,
  });
  final String label;
  final IconData? icon;
  final VoidCallback? onPressed;
  final BtnVariant variant;
  final bool compact;
  final bool loading;

  @override
  State<LButton> createState() => _LButtonState();
}

class _LButtonState extends State<LButton> {
  bool _hover = false, _down = false;

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final enabled = widget.onPressed != null && !widget.loading;
    Color bg, fg, bd = Colors.transparent;
    switch (widget.variant) {
      case BtnVariant.solid:
        bg = _hover ? c.actionHover : c.action;
        fg = c.actionFg;
      case BtnVariant.neutral:
        bg = _hover ? c.bgSubtle : c.bgCard;
        fg = c.text1;
        bd = c.borderStrong;
      case BtnVariant.quiet:
        bg = _hover ? c.bgSubtle : Colors.transparent;
        fg = c.text1;
      case BtnVariant.ghost:
        bg = _hover ? c.bgSubtle : Colors.transparent;
        fg = _hover ? c.text1 : c.text2;
      case BtnVariant.destructive:
        bg = c.danger;
        fg = Colors.white;
    }
    final style = Type.body.copyWith(
        color: fg, fontWeight: widget.variant == BtnVariant.solid ? FontWeight.w600 : FontWeight.w500);
    return MouseRegion(
      cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: enabled ? (_) => setState(() => _down = true) : null,
        onTapCancel: () => setState(() => _down = false),
        onTapUp: (_) => setState(() => _down = false),
        onTap: enabled ? widget.onPressed : null,
        child: AnimatedScale(
          scale: _down ? .96 : 1,
          duration: const Duration(milliseconds: 100),
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              height: widget.compact ? 28 : 32,
              padding: EdgeInsets.symmetric(horizontal: widget.compact ? 12 : 14),
              decoration: BoxDecoration(
                color: bg,
                borderRadius: BorderRadius.circular(Radii.control),
                border: Border.all(color: bd),
              ),
              child: Row(mainAxisSize: MainAxisSize.min, mainAxisAlignment: MainAxisAlignment.center, children: [
                if (widget.loading)
                  SizedBox(width: 14, height: 14, child: CircularProgressIndicator(strokeWidth: 2, color: fg))
                else if (widget.icon != null)
                  Icon(widget.icon, size: 16, color: fg),
                if (widget.icon != null || widget.loading) const SizedBox(width: 6),
                Text(widget.label, style: style),
              ]),
            ),
          ),
        ),
      ),
    );
  }
}

/// 28x28 icon button (cc-switch icon-compact).
class LIconButton extends StatefulWidget {
  const LIconButton({super.key, required this.icon, this.onPressed, this.tooltip, this.size = 28, this.iconSize = 16});
  final IconData icon;
  final VoidCallback? onPressed;
  final String? tooltip;
  final double size, iconSize;

  @override
  State<LIconButton> createState() => _LIconButtonState();
}

class _LIconButtonState extends State<LIconButton> {
  bool _hover = false;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final w = MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTap: widget.onPressed,
        child: Container(
          width: widget.size,
          height: widget.size,
          decoration: BoxDecoration(
              color: _hover ? c.bgSubtle : Colors.transparent, borderRadius: BorderRadius.circular(Radii.control)),
          child: Icon(widget.icon, size: widget.iconSize, color: _hover ? c.text1 : c.text2),
        ),
      ),
    );
    return widget.tooltip == null ? w : Tooltip(message: widget.tooltip!, child: w);
  }
}

/// Token-based selector shared by forms; no ripple or default underline.
class LSelectOption<T> {
  const LSelectOption(this.value, this.label, {this.enabled = true});
  final T value;
  final String label;
  final bool enabled;
}

class LSelect<T> extends StatelessWidget {
  const LSelect({super.key, required this.value, required this.options, this.onChanged});
  final T value;
  final List<LSelectOption<T>> options;
  final ValueChanged<T?>? onChanged;

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final selected = options.firstWhere((option) => option.value == value);
    return LayoutBuilder(builder: (context, box) => MenuAnchor(
      alignmentOffset: const Offset(0, 4),
      style: MenuStyle(
        alignment: AlignmentDirectional.bottomStart,
        backgroundColor: WidgetStatePropertyAll(c.bgCard),
        surfaceTintColor: const WidgetStatePropertyAll(Colors.transparent),
        elevation: const WidgetStatePropertyAll(0),
        padding: const WidgetStatePropertyAll(EdgeInsets.all(4)),
        fixedSize: WidgetStatePropertyAll(Size.fromWidth(box.maxWidth)),
        side: WidgetStatePropertyAll(BorderSide(color: c.borderStrong)),
        shape: WidgetStatePropertyAll(RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(Radii.control))),
      ),
      menuChildren: [
        for (final option in options)
          MenuItemButton(
            onPressed: onChanged == null || !option.enabled ? null : () => onChanged!(option.value),
            style: ButtonStyle(
              minimumSize: WidgetStatePropertyAll(Size(box.maxWidth - 8, 32)),
              maximumSize: WidgetStatePropertyAll(Size(box.maxWidth - 8, 32)),
              padding: const WidgetStatePropertyAll(EdgeInsets.symmetric(horizontal: 8)),
              tapTargetSize: MaterialTapTargetSize.shrinkWrap,
              splashFactory: NoSplash.splashFactory,
              backgroundColor: WidgetStatePropertyAll(option.value == value ? c.bgSelected : Colors.transparent),
              overlayColor: WidgetStateProperty.resolveWith((states) =>
                states.contains(WidgetState.hovered) || states.contains(WidgetState.focused)
                  ? c.bgSubtle : Colors.transparent),
              shape: WidgetStatePropertyAll(RoundedRectangleBorder(
                borderRadius: BorderRadius.circular(Radii.control))),
            ),
            trailingIcon: option.value == value
              ? Icon(LucideIcons.check, size: 14, color: c.text2)
              : const SizedBox(width: 14),
            child: Text(option.label, overflow: TextOverflow.ellipsis,
              style: Type.body.copyWith(color: option.enabled ? c.text1 : c.text3)),
          ),
      ],
      builder: (context, controller, child) => SizedBox(
        height: 32,
        width: box.maxWidth,
        child: TextButton(
          onPressed: onChanged == null ? null : () {
            controller.isOpen ? controller.close() : controller.open();
          },
          style: ButtonStyle(
            padding: const WidgetStatePropertyAll(EdgeInsets.symmetric(horizontal: 10)),
            minimumSize: const WidgetStatePropertyAll(Size.zero),
            tapTargetSize: MaterialTapTargetSize.shrinkWrap,
            splashFactory: NoSplash.splashFactory,
            backgroundColor: WidgetStatePropertyAll(c.bgCard),
            overlayColor: WidgetStatePropertyAll(c.bgSubtle),
            side: WidgetStatePropertyAll(BorderSide(color: controller.isOpen ? c.action : c.borderStrong)),
            shape: WidgetStatePropertyAll(RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(Radii.control))),
          ),
          child: Row(children: [
            Expanded(child: Text(selected.label, overflow: TextOverflow.ellipsis,
              style: Type.body.copyWith(color: onChanged == null ? c.text3 : c.text1))),
            Icon(controller.isOpen ? LucideIcons.chevronUp : LucideIcons.chevronDown,
              size: 16, color: c.text3),
          ]),
        ),
      ),
    ));
  }
}

/// Text input: 32px high, radius 6, 1px border-strong, focus ring in the theme color.
class LTextField extends StatelessWidget {
  const LTextField({
    super.key,
    this.controller,
    this.hint,
    this.obscure = false,
    this.prefix,
    this.onSubmitted,
    this.onChanged,
    this.autofocus = false,
    this.height = 32,
    this.maxLines = 1,
    this.focusNode,
    this.fillColor,
  });
  final TextEditingController? controller;
  final String? hint;
  final bool obscure, autofocus;
  final Widget? prefix;
  final ValueChanged<String>? onSubmitted, onChanged;
  final double height;
  final int? maxLines;
  final FocusNode? focusNode;
  final Color? fillColor;

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    OutlineInputBorder b(Color col, [double w = 1]) => OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.control), borderSide: BorderSide(color: col, width: w));
    return SizedBox(
      height: maxLines == 1 ? height : null,
      child: TextField(
        controller: controller,
        focusNode: focusNode,
        obscureText: obscure,
        autofocus: autofocus,
        maxLines: obscure ? 1 : maxLines,
        onSubmitted: onSubmitted,
        onChanged: onChanged,
        style: Type.body.copyWith(color: c.text1),
        cursorColor: c.action,
        decoration: InputDecoration(
          isDense: true,
          hintText: hint,
          hintStyle: Type.body.copyWith(color: c.text3),
          filled: true,
          fillColor: fillColor ?? c.bgCard,
          prefixIcon: prefix,
          prefixIconConstraints: const BoxConstraints(minWidth: 32, minHeight: 32),
          contentPadding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
          enabledBorder: b(c.borderStrong),
          focusedBorder: b(c.action, 1.5),
          border: b(c.borderStrong),
        ),
      ),
    );
  }
}

/// Pill badge: 18px high, radius full, 11px text.
class LBadge extends StatelessWidget {
  const LBadge(this.text, {super.key, this.bg, this.fg, this.outline = false});
  final String text;
  final Color? bg, fg;
  final bool outline;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    // Hug the text: inside a Wrap a centred Container would stretch to the full row width.
    return Container(
      height: 18,
      padding: const EdgeInsets.symmetric(horizontal: 6),
      decoration: BoxDecoration(
        color: outline ? null : (bg ?? c.bgSubtle),
        borderRadius: BorderRadius.circular(9),
        border: outline ? Border.all(color: c.borderStrong) : null,
      ),
      child: Center(widthFactor: 1, child: Text(text, style: Type.badge.copyWith(color: fg ?? c.text2, height: 1))),
    );
  }
}

/// Device avatar: white tile with 1px border like cc-switch provider icons.
class DeviceGlyph extends StatelessWidget {
  const DeviceGlyph({super.key, required this.type, this.size = 40, this.online});
  final String type;
  final double size;
  final bool? online;

  static IconData iconFor(String type) => switch (type) {
        'android' || 'ios' => LucideIcons.smartphone,
        'linux' => LucideIcons.terminal,
        'web' => LucideIcons.globe,
        'macos' => LucideIcons.laptop,
        _ => LucideIcons.monitor,
      };

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return SizedBox(
      width: size,
      height: size,
      child: Stack(clipBehavior: Clip.none, children: [
        Container(
          width: size,
          height: size,
          decoration: BoxDecoration(
            color: c.bgCard,
            borderRadius: BorderRadius.circular(Radii.panel),
            border: Border.all(color: c.border),
          ),
          child: Icon(iconFor(type), size: size * .5, color: c.text1),
        ),
        if (online != null)
          Positioned(
            right: -2,
            bottom: -2,
            child: Container(
              width: 11,
              height: 11,
              decoration: BoxDecoration(
                color: online! ? c.success : c.controlOff,
                shape: BoxShape.circle,
                border: Border.all(color: c.bgSidebar, width: 2),
              ),
            ),
          ),
      ]),
    );
  }
}

/// Hover/selected row used by list items (cc-switch nav item: radius 6, hover bg-subtle, selected bg-selected).
class HoverRow extends StatefulWidget {
  const HoverRow({super.key, required this.child, this.selected = false, this.onTap, this.padding, this.radius = Radii.control, this.onSecondaryTap});
  final Widget child;
  final bool selected;
  final VoidCallback? onTap, onSecondaryTap;
  final EdgeInsets? padding;
  final double radius;

  @override
  State<HoverRow> createState() => _HoverRowState();
}

class _HoverRowState extends State<HoverRow> {
  bool _hover = false;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      hitTestBehavior: HitTestBehavior.opaque,
      onEnter: (_) { if (!_hover) setState(() => _hover = true); },
      onHover: (_) { if (!_hover) setState(() => _hover = true); },
      onExit: (_) { if (_hover) setState(() => _hover = false); },
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onTap,
        onSecondaryTap: widget.onSecondaryTap,
        child: Container(
          padding: widget.padding,
          decoration: BoxDecoration(
            color: widget.selected ? c.bgSelected : (_hover ? c.bgSubtle : Colors.transparent),
            borderRadius: BorderRadius.circular(widget.radius),
          ),
          child: widget.child,
        ),
      ),
    );
  }
}

/// Content-area page header: 52px high, bottom border, title left / actions right (AppPageHeader).
class PageHeader extends StatelessWidget {
  const PageHeader({super.key, required this.title, this.subtitle, this.leading, this.actions = const []});
  final String title;
  final Widget? leading;
  final String? subtitle;
  final List<Widget> actions;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    // Pushed detail pages on narrow screens get a back button.
    final back = isNarrow(context) && (ModalRoute.of(context)?.canPop ?? false);
    return DragArea(child: Container(
      height: 52,
      padding: EdgeInsets.only(left: back ? 8 : 24, right: 16),
      decoration: BoxDecoration(border: Border(bottom: BorderSide(color: c.border))),
      child: Row(children: [
        if (back) ...[
          LIconButton(icon: LucideIcons.chevronLeft, tooltip: '返回', size: 32, iconSize: 20, onPressed: () => Navigator.maybePop(context)),
          const SizedBox(width: 4),
        ],
        if (leading != null) ...[leading!, const SizedBox(width: 10)],
        Expanded(
          child: Row(children: [
            Flexible(child: Text(title, style: Type.title.copyWith(color: c.text1), overflow: TextOverflow.ellipsis)),
            if (subtitle != null) ...[
              const SizedBox(width: 8),
              Text(subtitle!, style: Type.body.copyWith(color: c.text3)),
            ],
          ]),
        ),
        for (final a in actions) ...[const SizedBox(width: 8), a],
        if (hasCustomWindowControls || debugShowWindowControls) ...[
          const SizedBox(width: 10),
          Container(width: 1, height: 16, color: c.border),
          const SizedBox(width: 8),
          const WindowControls(),
        ],
      ]),
    ));
  }
}

/// Card/panel: radius 10, 1px border, white surface; hover lifts border + shadow-sm.
class PanelCard extends StatefulWidget {
  const PanelCard({super.key, required this.child, this.active = false, this.padding = const EdgeInsets.all(16)});
  final Widget child;
  final bool active;
  final EdgeInsets padding;
  @override
  State<PanelCard> createState() => _PanelCardState();
}

class _PanelCardState extends State<PanelCard> {
  bool _hover = false;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 150),
        padding: widget.padding,
        decoration: BoxDecoration(
          color: widget.active ? c.directSoft : c.bgCard,
          borderRadius: BorderRadius.circular(Radii.panel),
          border: Border.all(color: widget.active ? c.direct : (_hover ? c.borderStrong : c.border)),
          boxShadow: _hover && !widget.active ? c.shadowSm : null,
        ),
        child: widget.child,
      ),
    );
  }
}

Future<void> copyText(BuildContext context, String text) async {
  await Clipboard.setData(ClipboardData(text: text));
}

/// Dialog shell: radius 14, border, title + body + right-aligned actions (cc-switch dialog).
class LDialog extends StatelessWidget {
  const LDialog({super.key, required this.title, required this.body, required this.confirm, required this.onConfirm, this.danger = false});
  final String title, confirm;
  final Widget body;
  final VoidCallback onConfirm;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return Dialog(
      backgroundColor: c.bgCard,
      elevation: 0,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Radii.dialog), side: BorderSide(color: c.border)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 400),
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
            Text(title, style: Type.title.copyWith(color: c.text1)),
            const SizedBox(height: 14),
            body,
            const SizedBox(height: 20),
            Row(mainAxisAlignment: MainAxisAlignment.end, children: [
              LButton(label: '取消', onPressed: () => Navigator.pop(context)),
              const SizedBox(width: 8),
              LButton(label: confirm, variant: danger ? BtnVariant.destructive : BtnVariant.solid, onPressed: onConfirm),
            ]),
          ]),
        ),
      ),
    );
  }
}

class ConfirmDialog extends StatelessWidget {
  const ConfirmDialog({super.key, required this.title, required this.message, required this.confirmLabel, this.destructive = false});
  final String title, message, confirmLabel;
  final bool destructive;

  @override
  Widget build(BuildContext context) => LDialog(
        title: title,
        body: Text(message, style: Type.body.copyWith(color: context.c.text2)),
        confirm: confirmLabel,
        danger: destructive,
        onConfirm: () => Navigator.pop(context, true),
      );
}

/// Below this width the three columns collapse to one (phones / narrow windows).
const narrowBreakpoint = 720.0;

bool isNarrow(BuildContext context) => MediaQuery.sizeOf(context).width < narrowBreakpoint;

/// Window drag handle for the hidden title bar (desktop only). Double-click toggles maximise on
/// Windows/Linux. Taps are never delayed: the double-click is detected from raw pointer events,
/// not a double-tap recognizer (an ancestor recognizer would make every inner button wait ~300ms).
class DragArea extends StatefulWidget {
  const DragArea({super.key, required this.child});
  final Widget child;
  @override
  State<DragArea> createState() => _DragAreaState();
}

class _DragAreaState extends State<DragArea> {
  DateTime _lastDown = DateTime.fromMillisecondsSinceEpoch(0);
  Offset _lastPos = Offset.zero;

  void _down(PointerDownEvent e) {
    if (!hasCustomWindowControls) return;
    final now = DateTime.now();
    if (now.difference(_lastDown) < const Duration(milliseconds: 350) && (e.position - _lastPos).distance < 8) {
      _lastDown = DateTime.fromMillisecondsSinceEpoch(0);
      _toggleMaximize();
    } else {
      _lastDown = now;
      _lastPos = e.position;
    }
  }

  Future<void> _toggleMaximize() async => await windowManager.isMaximized() ? windowManager.unmaximize() : windowManager.maximize();

  @override
  Widget build(BuildContext context) {
    if (!windowShellActive) return widget.child;
    return Listener(
      behavior: HitTestBehavior.translucent,
      onPointerDown: _down,
      child: GestureDetector(behavior: HitTestBehavior.translucent, onPanStart: (_) => windowManager.startDragging(), child: widget.child),
    );
  }
}

/// Test hook: show the custom window buttons even on the macOS test host.
bool debugShowWindowControls = false;

/// Minimise / maximise-restore / close for the frameless window (Windows, Linux).
class WindowControls extends StatefulWidget {
  const WindowControls({super.key});
  @override
  State<WindowControls> createState() => _WindowControlsState();
}

class _WindowControlsState extends State<WindowControls> with WindowListener {
  bool _maximized = false;

  @override
  void initState() {
    super.initState();
    if (!debugShowWindowControls) {
      windowManager.addListener(this);
      windowManager.isMaximized().then((v) => mounted ? setState(() => _maximized = v) : null);
    }
  }

  @override
  void dispose() {
    if (!debugShowWindowControls) windowManager.removeListener(this);
    super.dispose();
  }

  @override
  void onWindowMaximize() => setState(() => _maximized = true);
  @override
  void onWindowUnmaximize() => setState(() => _maximized = false);
  @override
  void onWindowEnterFullScreen() => setState(() => _maximized = true);
  @override
  void onWindowLeaveFullScreen() => setState(() => _maximized = false);

  @override
  Widget build(BuildContext context) => Row(mainAxisSize: MainAxisSize.min, children: [
        _WinBtn(icon: LucideIcons.minus, tooltip: '最小化', onTap: () => windowManager.minimize()),
        _WinBtn(
            icon: _maximized ? LucideIcons.copy : LucideIcons.square,
            tooltip: _maximized ? '还原' : '最大化',
            iconSize: _maximized ? 13 : 12,
            onTap: () async => await windowManager.isMaximized() ? windowManager.unmaximize() : windowManager.maximize()),
        _WinBtn(icon: LucideIcons.x, tooltip: '关闭', danger: true, onTap: () => windowManager.close()),
      ]);
}

class _WinBtn extends StatefulWidget {
  const _WinBtn({required this.icon, required this.tooltip, required this.onTap, this.danger = false, this.iconSize = 15});
  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;
  final bool danger;
  final double iconSize;
  @override
  State<_WinBtn> createState() => _WinBtnState();
}

class _WinBtnState extends State<_WinBtn> {
  bool _hover = false;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return Tooltip(
      message: widget.tooltip,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hover = true),
        onExit: (_) => setState(() => _hover = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onTap,
          child: Container(
            width: 32,
            height: 28,
            margin: const EdgeInsets.only(left: 2),
            decoration: BoxDecoration(
              color: _hover ? (widget.danger ? c.dangerSoft : c.bgSubtle) : Colors.transparent,
              borderRadius: BorderRadius.circular(Radii.control),
            ),
            child: Icon(widget.icon, size: widget.iconSize, color: _hover && widget.danger ? c.dangerText : (_hover ? c.text1 : c.text2)),
          ),
        ),
      ),
    );
  }
}

/// Toggle: 36x20 track, orange when on (cc-switch Switch).
class LSwitch extends StatelessWidget {
  const LSwitch({super.key, required this.value, required this.onChanged});
  final bool value;
  final ValueChanged<bool>? onChanged;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onChanged == null ? null : () => onChanged!(!value),
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          width: 36,
          height: 20,
          padding: const EdgeInsets.all(2),
          alignment: value ? Alignment.centerRight : Alignment.centerLeft,
          decoration: BoxDecoration(color: value ? c.action : c.controlOff, borderRadius: BorderRadius.circular(10)),
          child: Container(width: 16, height: 16, decoration: const BoxDecoration(color: Colors.white, shape: BoxShape.circle)),
        ),
      ),
    );
  }
}
