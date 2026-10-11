import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../theme/tokens.dart';
import 'widgets.dart';

/// 8px status dot. [color] picks the meaning (success / warning / danger / controlOff).
class StatusDot extends StatelessWidget {
  const StatusDot({super.key, required this.color, this.size = 8});
  final Color color;
  final double size;
  @override
  Widget build(BuildContext context) =>
      Container(width: size, height: size, decoration: BoxDecoration(color: color, shape: BoxShape.circle));
}

/// Metric card: big number + caption (+ optional hint line).
class MetricCard extends StatelessWidget {
  const MetricCard({super.key, required this.label, required this.value, this.hint, this.icon});
  final String label, value;
  final String? hint;
  final IconData? icon;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return PanelCard(
      child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Row(children: [
          if (icon != null) ...[Icon(icon, size: 16, color: c.text3), const SizedBox(width: 6)],
          Text(label, style: Type.caption.copyWith(color: c.text3)),
        ]),
        const SizedBox(height: 8),
        Text(value, style: Type.metric.copyWith(color: c.text1)),
        // Always reserve the hint line so cards in one row have equal height.
        const SizedBox(height: 4),
        Text(hint ?? ' ', style: Type.caption.copyWith(color: c.text3)),
      ]),
    );
  }
}

/// Centred empty / error placeholder with an optional action.
class EmptyState extends StatelessWidget {
  const EmptyState({super.key, required this.icon, required this.title, this.message, this.action});
  final IconData icon;
  final String title;
  final String? message;
  final Widget? action;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 360),
          child: Column(mainAxisSize: MainAxisSize.min, children: [
            Icon(icon, size: 28, color: c.text3),
            const SizedBox(height: 12),
            Text(title, style: Type.strong.copyWith(color: c.text1), textAlign: TextAlign.center),
            if (message != null) ...[
              const SizedBox(height: 6),
              Text(message!, style: Type.body.copyWith(color: c.text3), textAlign: TextAlign.center),
            ],
            if (action != null) ...[const SizedBox(height: 16), action!],
          ]),
        ),
      ),
    );
  }
}

/// Read-only monospace text with a copy button (install commands, addresses).
class CopyField extends StatelessWidget {
  const CopyField({super.key, required this.text, this.label});
  final String text;
  final String? label;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
      if (label != null) ...[Text(label!, style: Type.caption.copyWith(color: c.text3)), const SizedBox(height: 4)],
      Container(
        padding: const EdgeInsets.fromLTRB(10, 6, 4, 6),
        decoration: BoxDecoration(
          color: c.bgSubtle,
          borderRadius: BorderRadius.circular(Radii.control),
          border: Border.all(color: c.border),
        ),
        child: Row(children: [
          Expanded(
            child: SelectableText(text,
                style: Type.caption.copyWith(color: c.text1, fontFamily: 'monospace', fontFamilyFallback: Type.monoFallback)),
          ),
          LIconButton(
            icon: LucideIcons.copy,
            tooltip: '复制',
            onPressed: () async {
              await Clipboard.setData(ClipboardData(text: text));
              if (context.mounted) showToast(context, '已复制');
            },
          ),
        ]),
      ),
    ]);
  }
}

/// Section heading used inside detail pages.
class SectionTitle extends StatelessWidget {
  const SectionTitle(this.text, {super.key, this.trailing});
  final String text;
  final Widget? trailing;
  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.only(bottom: 8),
        child: Row(children: [
          Expanded(child: Text(text, style: Type.section.copyWith(color: context.c.text1))),
          ?trailing,
        ]),
      );
}

/// Label / value row for detail cards.
class InfoRow extends StatelessWidget {
  const InfoRow(this.label, this.value, {super.key, this.mono = false});
  final String label, value;
  final bool mono;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 5),
      child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
        SizedBox(width: 96, child: Text(label, style: Type.body.copyWith(color: c.text3))),
        Expanded(
          child: SelectableText(value,
              style: Type.body.copyWith(
                  color: c.text1, fontFamily: mono ? 'monospace' : null, fontFamilyFallback: mono ? Type.monoFallback : null)),
        ),
      ]),
    );
  }
}

/// Floating toast (SnackBar) in the project style: 360 wide, reason first for errors.
void showToast(BuildContext context, String message, {bool error = false}) {
  final c = context.c;
  final m = ScaffoldMessenger.maybeOf(context);
  if (m == null) return;
  m.hideCurrentSnackBar();
  m.showSnackBar(SnackBar(
    behavior: SnackBarBehavior.floating,
    width: 360,
    backgroundColor: error ? c.dangerSoft : c.bgCard,
    elevation: 0,
    shape: RoundedRectangleBorder(
      borderRadius: BorderRadius.circular(Radii.control),
      side: BorderSide(color: error ? c.danger : c.borderStrong),
    ),
    content: Text(message, style: Type.body.copyWith(color: error ? c.dangerText : c.text1)),
    duration: const Duration(seconds: 4),
  ));
}

/// Asks for a reason (required: the controller audits every change). Returns null when cancelled.
Future<String?> askReason(BuildContext context,
    {required String title, required String message, String confirm = '确认', bool destructive = false}) {
  final ctl = TextEditingController();
  return showDialog<String>(
    context: context,
    builder: (ctx) => StatefulBuilder(
      builder: (ctx, setState) => DialogShell(
        title: title,
        content: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(message, style: Type.body.copyWith(color: ctx.c.text2)),
          const SizedBox(height: 12),
          LTextField(controller: ctl, hint: '操作理由（必填，会写入审计）', autofocus: true, onChanged: (_) => setState(() {})),
        ]),
        actions: [
          LButton(label: '取消', onPressed: () => Navigator.pop(ctx)),
          LButton(
            label: confirm,
            variant: destructive ? BtnVariant.destructive : BtnVariant.solid,
            onPressed: ctl.text.trim().isEmpty ? null : () => Navigator.pop(ctx, ctl.text.trim()),
          ),
        ],
      ),
    ),
  );
}

/// Runs [f], showing any error as a toast.
Future<T?> guarded<T>(BuildContext context, Future<T> Function() f) async {
  try {
    return await f();
  } on Exception catch (e) {
    if (context.mounted) showToast(context, e.toString(), error: true);
    return null;
  }
}

/// Dialog frame with free-form content and a right-aligned action row (radius 14, shadow-lg border).
class DialogShell extends StatelessWidget {
  const DialogShell({super.key, required this.title, required this.content, required this.actions, this.maxWidth = 440});
  final String title;
  final Widget content;
  final List<Widget> actions;
  final double maxWidth;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return Dialog(
      backgroundColor: c.bgCard,
      elevation: 0,
      insetPadding: const EdgeInsets.all(16),
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Radii.dialog), side: BorderSide(color: c.border)),
      child: ConstrainedBox(
        constraints: BoxConstraints(maxWidth: maxWidth),
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: SingleChildScrollView(
            child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(title, style: Type.title.copyWith(color: c.text1)),
              const SizedBox(height: 14),
              content,
              const SizedBox(height: 20),
              Row(mainAxisAlignment: MainAxisAlignment.end, children: [
                for (var i = 0; i < actions.length; i++) ...[if (i > 0) const SizedBox(width: 8), actions[i]],
              ]),
            ]),
          ),
        ),
      ),
    );
  }
}

/// Renders an [AsyncValue]: keeps showing the previous data while refreshing; errors get a retry button.
class AsyncView<T> extends StatelessWidget {
  const AsyncView({super.key, required this.value, required this.builder, this.onRetry});
  final AsyncValue<T> value;
  final Widget Function(T data) builder;
  final VoidCallback? onRetry;
  @override
  Widget build(BuildContext context) {
    if (value.hasValue) return builder(value.requireValue);
    if (value.hasError) {
      return EmptyState(
        icon: LucideIcons.triangleAlert,
        title: '加载失败',
        message: value.error.toString(),
        action: onRetry == null ? null : LButton(label: '重试', onPressed: onRetry),
      );
    }
    return const Center(child: SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2)));
  }
}
