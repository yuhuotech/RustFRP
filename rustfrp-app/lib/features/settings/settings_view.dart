import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';
import '../shell/shell.dart';

class SettingsPage {
  const SettingsPage(this.id, this.icon, this.label, {this.minRole = Role.viewer});
  final String id, label;
  final IconData icon;
  final Role minRole;
}

const settingsPages = [
  SettingsPage('account', LucideIcons.user, '账号'),
  SettingsPage('appearance', LucideIcons.palette, '外观'),
  SettingsPage('tokens', LucideIcons.keyRound, '接入令牌', minRole: Role.admin),
  SettingsPage('admins', LucideIcons.shieldCheck, '管理员', minRole: Role.owner),
  SettingsPage('about', LucideIcons.info, '关于'),
];

class SettingsList extends ConsumerWidget {
  const SettingsList({super.key, this.narrow = false});
  final bool narrow;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final selected = ref.watch(selectedSettingProvider) ?? 'account';
    return Column(children: [
      if (narrow) const PageHeader(title: '设置'),
      Expanded(
        child: ListView(padding: const EdgeInsets.all(8), children: [
          for (final p in settingsPages.where((p) => role.atLeast(p.minRole)))
            Padding(
              padding: const EdgeInsets.only(bottom: 2),
              child: HoverRow(
                key: Key('settings-${p.id}'),
                selected: !narrow && selected == p.id,
                padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
                onTap: () {
                  ref.read(selectedSettingProvider.notifier).select(p.id);
                  if (narrow) pushDetail(context, SettingsDetail(page: p.id));
                },
                child: Row(children: [
                  Icon(p.icon, size: 16, color: c.text2),
                  const SizedBox(width: 10),
                  Expanded(child: Text(p.label, style: Type.body.copyWith(color: c.text1))),
                ]),
              ),
            ),
        ]),
      ),
    ]);
  }
}

class SettingsDetailHost extends ConsumerWidget {
  const SettingsDetailHost({super.key});
  @override
  Widget build(BuildContext context, WidgetRef ref) => SettingsDetail(page: ref.watch(selectedSettingProvider) ?? 'account');
}

class SettingsDetail extends ConsumerWidget {
  const SettingsDetail({super.key, required this.page});
  final String page;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final p = settingsPages.firstWhere((x) => x.id == page, orElse: () => settingsPages.first);
    return Column(children: [
      PageHeader(title: p.label),
      Expanded(
        child: ListView(padding: const EdgeInsets.all(24), children: [
          switch (p.id) {
            'appearance' => const _Appearance(),
            'tokens' => const _Tokens(),
            'admins' => const _Admins(),
            'about' => const _About(),
            _ => const _Account(),
          },
        ]),
      ),
    ]);
  }
}

class _Account extends ConsumerWidget {
  const _Account();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ref.watch(sessionProvider);
    final a = s.admin;
    return Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
      PanelCard(
        child: Column(children: [
          InfoRow('用户名', a?.name ?? '—'),
          InfoRow('角色', a?.role.label ?? '—'),
          if (s.serverUrl.isNotEmpty) InfoRow('控制器', s.serverUrl, mono: true),
        ]),
      ),
      const SizedBox(height: 16),
      LButton(key: const Key('logout'), label: '退出登录', icon: LucideIcons.logOut, onPressed: () => ref.read(sessionProvider.notifier).logout()),
    ]);
  }
}

class _Appearance extends ConsumerWidget {
  const _Appearance();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final mode = ref.watch(themeModeProvider);
    return Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
      Text('主题', style: Type.caption.copyWith(color: context.c.text3)),
      const SizedBox(height: 6),
      Wrap(spacing: 8, children: [
        for (final m in [(ThemeMode.system, '跟随系统'), (ThemeMode.light, '浅色'), (ThemeMode.dark, '深色')])
          LButton(
            key: Key('theme-${m.$1.name}'),
            label: m.$2,
            compact: true,
            variant: mode == m.$1 ? BtnVariant.solid : BtnVariant.neutral,
            onPressed: () => ref.read(themeModeProvider.notifier).set(m.$1),
          ),
      ]),
    ]);
  }
}

class _Tokens extends ConsumerWidget {
  const _Tokens();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final tokens = ref.watch(tokensProvider);
    return AsyncViewInline<List<EnrollToken>>(
      value: tokens,
      onRetry: () => ref.invalidate(tokensProvider),
      builder: (list) {
        if (list.isEmpty) return Text('还没有创建过接入令牌。在「客户端」页点击「接入新客户端」即可生成。', style: Type.body.copyWith(color: c.text3));
        final now = DateTime.now().millisecondsSinceEpoch;
        return Column(children: [
          for (final t in list)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: PanelCard(
                key: Key('token-${t.id}'),
                child: Row(children: [
                  Expanded(
                    child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                      Text(t.name, style: Type.strong.copyWith(color: c.text1)),
                      Text('${t.createdBy} · ${fullTime(t.createdAt)}', style: Type.caption.copyWith(color: c.text3)),
                    ]),
                  ),
                  LBadge(switch (t.status(now)) { 'used' => '已使用', 'revoked' => '已撤销', 'expired' => '已过期', _ => '待使用' },
                      bg: t.status(now) == 'pending' ? c.actionSoft : null, fg: t.status(now) == 'pending' ? c.actionText : null),
                  if (t.status(now) == 'pending') ...[
                    const SizedBox(width: 8),
                    LButton(
                      label: '撤销',
                      compact: true,
                      onPressed: () async {
                        final reason = await askReason(context, title: '撤销接入令牌', message: '撤销后，用它生成的安装命令将失效。', confirm: '撤销', destructive: true);
                        if (reason == null || !context.mounted) return;
                        await guarded(context, () => ref.read(actionsProvider).revokeToken(t.id, reason));
                      },
                    ),
                  ],
                ]),
              ),
            ),
        ]);
      },
    );
  }
}

class _Admins extends ConsumerWidget {
  const _Admins();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final admins = ref.watch(adminsProvider);
    return AsyncViewInline<List<Admin>>(
      value: admins,
      onRetry: () => ref.invalidate(adminsProvider),
      builder: (list) => Column(children: [
        for (final a in list)
          Padding(
            padding: const EdgeInsets.only(bottom: 8),
            child: PanelCard(
              child: Row(children: [
                Expanded(child: Text(a.name, style: Type.strong.copyWith(color: c.text1))),
                LBadge(a.role.label),
                if (a.disabledAt != null) ...[const SizedBox(width: 6), LBadge('已禁用', bg: c.dangerSoft, fg: c.dangerText)],
              ]),
            ),
          ),
        Align(
          alignment: Alignment.centerLeft,
          child: Text('管理员账号只能在服务器上用 rustfrp-server admin 命令创建、禁用或重置密码。', style: Type.caption.copyWith(color: c.text3)),
        ),
      ]),
    );
  }
}

class _About extends ConsumerWidget {
  const _About();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final v = ref.watch(overviewProvider).asData?.value.version ?? '';
    return Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
      const BrandLogo(size: 48),
      const SizedBox(height: 12),
      Text('RustFRP', style: Type.page.copyWith(color: c.text1)),
      const SizedBox(height: 4),
      Text('用 Rust 实现、极致省内存、可视化管理的 FRP 内网穿透。', style: Type.body.copyWith(color: c.text2)),
      const SizedBox(height: 16),
      PanelCard(child: Column(children: [InfoRow('控制器版本', v.isEmpty ? '—' : v), InfoRow('面板版本', '0.1.0')])),
    ]);
  }
}

/// [AsyncView] without the centred full-page layout (for content inside a ListView).
class AsyncViewInline<T> extends StatelessWidget {
  const AsyncViewInline({super.key, required this.value, required this.builder, this.onRetry});
  final AsyncValue<T> value;
  final Widget Function(T) builder;
  final VoidCallback? onRetry;
  @override
  Widget build(BuildContext context) {
    if (value.hasValue) return builder(value.requireValue);
    if (value.hasError) {
      return Row(children: [
        Expanded(child: Text(value.error.toString(), style: Type.body.copyWith(color: context.c.dangerText))),
        if (onRetry != null) LButton(label: '重试', compact: true, onPressed: onRetry),
      ]);
    }
    return const Padding(padding: EdgeInsets.all(8), child: SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2)));
  }
}
