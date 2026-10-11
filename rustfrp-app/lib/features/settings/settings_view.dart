import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/local_agent.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';
import '../shell/shell.dart';
import 'api_tokens_view.dart';

class SettingsPage {
  const SettingsPage(
    this.id,
    this.icon,
    this.label, {
    this.minRole = Role.viewer,
  });
  final String id, label;
  final IconData icon;
  final Role minRole;
}

const settingsPages = [
  SettingsPage('account', LucideIcons.user, '账号'),
  SettingsPage('appearance', LucideIcons.palette, '外观'),
  SettingsPage('tokens', LucideIcons.keyRound, '接入令牌', minRole: Role.admin),
  SettingsPage('logs', LucideIcons.scrollText, '控制器日志', minRole: Role.admin),
  SettingsPage('api_tokens', LucideIcons.key, 'API Token', minRole: Role.owner),
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
    return Column(
      children: [
        if (narrow) const PageHeader(title: '设置'),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.all(8),
            children: [
              for (final p in settingsPages.where(
                (p) => role.atLeast(p.minRole),
              ))
                Padding(
                  padding: const EdgeInsets.only(bottom: 2),
                  child: HoverRow(
                    key: Key('settings-${p.id}'),
                    selected: !narrow && selected == p.id,
                    padding: const EdgeInsets.symmetric(
                      horizontal: 10,
                      vertical: 8,
                    ),
                    onTap: () {
                      ref.read(selectedSettingProvider.notifier).select(p.id);
                      if (narrow) {
                        pushDetail(context, SettingsDetail(page: p.id));
                      }
                    },
                    child: Row(
                      children: [
                        Icon(p.icon, size: 16, color: c.text2),
                        const SizedBox(width: 10),
                        Expanded(
                          child: Text(
                            p.label,
                            style: Type.body.copyWith(color: c.text1),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
            ],
          ),
        ),
      ],
    );
  }
}

/// Settings tabs use the workspace width; no second navigation column.
class SettingsWorkspace extends ConsumerWidget {
  const SettingsWorkspace({super.key});
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final visible = settingsPages
        .where((p) => role.atLeast(p.minRole))
        .toList();
    final selected = ref.watch(selectedSettingProvider) ?? 'account';
    final effective = visible.any((p) => p.id == selected)
        ? selected
        : 'account';
    return Column(
      children: [
        const PageHeader(title: '设置'),
        Align(
          alignment: Alignment.centerLeft,
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.fromLTRB(24, 12, 24, 0),
            child: Row(
              children: [
                for (final p in visible)
                  Padding(
                    padding: const EdgeInsets.only(right: 8),
                    child: LButton(
                      key: Key('settings-${p.id}'),
                      label: p.label,
                      icon: p.icon,
                      compact: true,
                      variant: effective == p.id
                          ? BtnVariant.quiet
                          : BtnVariant.ghost,
                      onPressed: () => ref
                          .read(selectedSettingProvider.notifier)
                          .select(p.id),
                    ),
                  ),
              ],
            ),
          ),
        ),
        Expanded(child: SettingsDetail(page: effective, showHeader: false)),
      ],
    );
  }
}

class SettingsDetail extends ConsumerWidget {
  const SettingsDetail({super.key, required this.page, this.showHeader = true});
  final String page;
  final bool showHeader;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final p = settingsPages.firstWhere(
      (x) => x.id == page,
      orElse: () => settingsPages.first,
    );
    return Column(
      children: [
        if (showHeader) PageHeader(title: p.label),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.all(24),
            children: [
              switch (p.id) {
                'appearance' => const _Appearance(),
                'tokens' => const _Tokens(),
                'api_tokens' => const ApiTokensView(),
                'logs' => const _ControllerLogs(),
                'admins' => const _Admins(),
                'about' => const _About(),
                _ => const _Account(),
              },
            ],
          ),
        ),
      ],
    );
  }
}

class _Account extends ConsumerWidget {
  const _Account();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ref.watch(sessionProvider);
    final a = s.admin;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        PanelCard(
          child: Column(
            children: [
              InfoRow('用户名', a?.name ?? '—'),
              InfoRow('角色', a?.role.label ?? '—'),
              if (s.serverUrl.isNotEmpty)
                InfoRow('控制器', s.serverUrl, mono: true),
            ],
          ),
        ),
        const SizedBox(height: 16),
        if (ref.watch(localAvailableProvider)) ...[
          LButton(
            key: const Key('local-mode'),
            label: '管理本机',
            icon: LucideIcons.monitor,
            onPressed: () => ref.read(localModeProvider.notifier).select(true),
          ),
          const SizedBox(height: 12),
        ],
        LButton(
          key: const Key('logout'),
          label: '退出登录',
          icon: LucideIcons.logOut,
          onPressed: () => ref.read(sessionProvider.notifier).logout(),
        ),
      ],
    );
  }
}

class _Appearance extends ConsumerWidget {
  const _Appearance();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final mode = ref.watch(themeModeProvider);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('主题', style: Type.caption.copyWith(color: context.c.text3)),
        const SizedBox(height: 6),
        Wrap(
          spacing: 8,
          children: [
            for (final m in [
              (ThemeMode.system, '跟随系统'),
              (ThemeMode.light, '浅色'),
              (ThemeMode.dark, '深色'),
            ])
              LButton(
                key: Key('theme-${m.$1.name}'),
                label: m.$2,
                compact: true,
                variant: mode == m.$1 ? BtnVariant.solid : BtnVariant.neutral,
                onPressed: () => ref.read(themeModeProvider.notifier).set(m.$1),
              ),
          ],
        ),
      ],
    );
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
        if (list.isEmpty) {
          return Text(
            '还没有创建过接入令牌。在「客户端」页点击「接入新客户端」即可生成。',
            style: Type.body.copyWith(color: c.text3),
          );
        }
        final now = DateTime.now().millisecondsSinceEpoch;
        return Column(
          children: [
            for (final t in list)
              Padding(
                padding: const EdgeInsets.only(bottom: 8),
                child: PanelCard(
                  key: Key('token-${t.id}'),
                  child: Row(
                    children: [
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(
                              t.name,
                              style: Type.strong.copyWith(color: c.text1),
                            ),
                            Text(
                              '${t.createdBy} · ${fullTime(t.createdAt)}',
                              style: Type.caption.copyWith(color: c.text3),
                            ),
                          ],
                        ),
                      ),
                      LBadge(
                        switch (t.status(now)) {
                          'used' => '已使用',
                          'revoked' => '已撤销',
                          'expired' => '已过期',
                          _ => '待使用',
                        },
                        bg: t.status(now) == 'pending' ? c.actionSoft : null,
                        fg: t.status(now) == 'pending' ? c.actionText : null,
                      ),
                      if (t.status(now) == 'pending') ...[
                        const SizedBox(width: 8),
                        LButton(
                          label: '撤销',
                          compact: true,
                          onPressed: () async {
                            final reason = await askReason(
                              context,
                              title: '撤销接入令牌',
                              message: '撤销后，用它生成的安装命令将失效。',
                              confirm: '撤销',
                              destructive: true,
                            );
                            if (reason == null || !context.mounted) return;
                            await guarded(
                              context,
                              () => ref
                                  .read(actionsProvider)
                                  .revokeToken(t.id, reason),
                            );
                          },
                        ),
                      ],
                    ],
                  ),
                ),
              ),
          ],
        );
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
    final me = ref.watch(sessionProvider).admin?.id;
    Future<void> change(
      Admin a, {
      Role? role,
      bool? disabled,
      required String title,
      required String message,
    }) async {
      final why = await askReason(
        context,
        title: title,
        message: message,
        destructive: disabled == true,
      );
      if (why == null || !context.mounted) return;
      await guarded(context, () async {
        await ref
            .read(apiProvider)
            .patchAdmin(a.id, role: role, disabled: disabled, reason: why);
        ref.invalidate(adminsProvider);
      });
    }

    return AsyncViewInline<List<Admin>>(
      value: admins,
      onRetry: () => ref.invalidate(adminsProvider),
      builder: (list) => Column(
        children: [
          Align(
            alignment: Alignment.centerLeft,
            child: LButton(
              key: const Key('new-admin'),
              label: '新增管理员',
              onPressed: () => showDialog<void>(
                context: context,
                builder: (_) => const _AdminDialog(),
              ),
            ),
          ),
          const SizedBox(height: 12),
          for (final a in list)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: PanelCard(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        Expanded(
                          child: Text(
                            a.name,
                            style: Type.strong.copyWith(color: c.text1),
                          ),
                        ),
                        LBadge(a.role.label),
                        if (a.disabledAt != null) ...[
                          const SizedBox(width: 6),
                          LBadge('已禁用', bg: c.dangerSoft, fg: c.dangerText),
                        ],
                      ],
                    ),
                    if (a.id != me) ...[
                      const SizedBox(height: 8),
                      Wrap(
                        spacing: 8,
                        runSpacing: 8,
                        crossAxisAlignment: WrapCrossAlignment.center,
                        children: [
                          SizedBox(
                            width: 120,
                            child: LSelect<Role>(
                              key: Key('admin-role-${a.name}'),
                              value: a.role,
                              options: [
                                for (final r in Role.values)
                                  LSelectOption(r, r.label),
                              ],
                              onChanged: (r) {
                                if (r == null || r == a.role) return;
                                change(
                                  a,
                                  role: r,
                                  title: '修改角色',
                                  message:
                                      '把 ${a.name} 的角色改为${r.label}，其现有会话会立即失效。',
                                );
                              },
                            ),
                          ),
                          LButton(
                            key: Key('admin-toggle-${a.name}'),
                            label: a.disabledAt == null ? '禁用' : '启用',
                            onPressed: () => change(
                              a,
                              disabled: a.disabledAt == null,
                              title: a.disabledAt == null ? '禁用管理员' : '启用管理员',
                              message: a.disabledAt == null
                                  ? '${a.name} 将立即退出并无法登录。'
                                  : '${a.name} 将可以重新登录。',
                            ),
                          ),
                        ],
                      ),
                    ],
                    const SizedBox(height: 8),
                    Align(
                      alignment: Alignment.centerLeft,
                      child: LButton(
                        key: Key('admin-passwd-${a.name}'),
                        label: '重置密码',
                        onPressed: () => showDialog<void>(
                          context: context,
                          builder: (_) => _AdminDialog(target: a),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          Align(
            alignment: Alignment.centerLeft,
            child: Text(
              '至少保留一位启用的所有者；不能修改自己的角色或状态。所有变更都会写入审计。',
              style: Type.caption.copyWith(color: c.text3),
            ),
          ),
        ],
      ),
    );
  }
}

/// 新增管理员（[target] 为空）或重置指定管理员的密码。
class _AdminDialog extends ConsumerStatefulWidget {
  const _AdminDialog({this.target});
  final Admin? target;
  @override
  ConsumerState<_AdminDialog> createState() => _AdminDialogState();
}

class _AdminDialogState extends ConsumerState<_AdminDialog> {
  final name = TextEditingController();
  final password = TextEditingController();
  final reason = TextEditingController();
  Role role = Role.operator;
  bool busy = false;

  @override
  void dispose() {
    name.dispose();
    password.dispose();
    reason.dispose();
    super.dispose();
  }

  bool get valid =>
      (widget.target != null || name.text.trim().isNotEmpty) &&
      password.text.length >= 10 &&
      reason.text.trim().isNotEmpty;

  Future<void> submit() async {
    if (busy || !valid) return;
    setState(() => busy = true);
    final t = widget.target;
    final api = ref.read(apiProvider);
    final ok = await guarded(context, () async {
      if (t == null) {
        await api.createAdmin(
          name: name.text.trim(),
          password: password.text,
          role: role,
          reason: reason.text.trim(),
        );
      } else {
        await api.patchAdmin(
          t.id,
          password: password.text,
          reason: reason.text.trim(),
        );
      }
      return true;
    });
    if (!mounted) return;
    setState(() => busy = false);
    if (ok == true) {
      ref.invalidate(adminsProvider);
      Navigator.pop(context);
    }
  }

  @override
  Widget build(BuildContext context) {
    final t = widget.target;
    return DialogShell(
      title: t == null ? '新增管理员' : '重置 ${t.name} 的密码',
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (t == null) ...[
            LTextField(
              key: const Key('admin-name'),
              controller: name,
              hint: '用户名',
              onChanged: (_) => setState(() {}),
            ),
            const SizedBox(height: 12),
            LSelect<Role>(
              key: const Key('admin-new-role'),
              value: role,
              options: [for (final r in Role.values) LSelectOption(r, r.label)],
              onChanged: busy ? null : (r) => setState(() => role = r ?? role),
            ),
            const SizedBox(height: 12),
          ],
          LTextField(
            key: const Key('admin-password'),
            controller: password,
            hint: '密码（至少 10 位）',
            obscure: true,
            onChanged: (_) => setState(() {}),
          ),
          const SizedBox(height: 12),
          LTextField(
            key: const Key('admin-reason'),
            controller: reason,
            hint: '操作理由（写入审计）',
            onChanged: (_) => setState(() {}),
          ),
        ],
      ),
      actions: [
        LButton(
          label: '取消',
          onPressed: busy ? null : () => Navigator.pop(context),
        ),
        LButton(
          key: const Key('admin-submit'),
          label: t == null ? '创建' : '重置',
          loading: busy,
          onPressed: valid && !busy ? submit : null,
        ),
      ],
    );
  }
}

class _About extends ConsumerWidget {
  const _About();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final v = ref.watch(overviewProvider).asData?.value.version ?? '';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const BrandLogo(size: 48),
        const SizedBox(height: 12),
        Text('RustFRP', style: Type.page.copyWith(color: c.text1)),
        const SizedBox(height: 4),
        Text(
          '用 Rust 实现、极致省内存、可视化管理的 FRP 内网穿透。',
          style: Type.body.copyWith(color: c.text2),
        ),
        const SizedBox(height: 16),
        PanelCard(
          child: Column(
            children: [
              InfoRow('控制器版本', v.isEmpty ? '—' : v),
              InfoRow('面板版本', '0.1.0'),
            ],
          ),
        ),
      ],
    );
  }
}

/// [AsyncView] without the centred full-page layout (for content inside a ListView).
class AsyncViewInline<T> extends StatelessWidget {
  const AsyncViewInline({
    super.key,
    required this.value,
    required this.builder,
    this.onRetry,
  });
  final AsyncValue<T> value;
  final Widget Function(T) builder;
  final VoidCallback? onRetry;
  @override
  Widget build(BuildContext context) {
    if (value.hasValue) return builder(value.requireValue);
    if (value.hasError) {
      return Row(
        children: [
          Expanded(
            child: Text(
              value.error.toString(),
              style: Type.body.copyWith(color: context.c.dangerText),
            ),
          ),
          if (onRetry != null)
            LButton(label: '重试', compact: true, onPressed: onRetry),
        ],
      );
    }
    return const Padding(
      padding: EdgeInsets.all(8),
      child: SizedBox(
        width: 16,
        height: 16,
        child: CircularProgressIndicator(strokeWidth: 2),
      ),
    );
  }
}

class _ControllerLogs extends ConsumerWidget {
  const _ControllerLogs();
  @override
  Widget build(BuildContext context, WidgetRef ref) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      LButton(
        label: '刷新',
        onPressed: () => ref.invalidate(controllerLogsProvider),
      ),
      const SizedBox(height: 12),
      Text(
        '最近 128 条日志，秘密相关消息已隐藏；刷新获取当前快照。',
        style: Type.caption.copyWith(color: context.c.text3),
      ),
      const SizedBox(height: 12),
      AsyncViewInline<List<String>>(
        value: ref.watch(controllerLogsProvider),
        onRetry: () => ref.invalidate(controllerLogsProvider),
        builder: (lines) => lines.isEmpty
            ? const Text('暂无日志')
            : Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  for (final line in lines)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 6),
                      child: SelectableText(
                        line,
                        style: Type.body.copyWith(color: context.c.text2),
                      ),
                    ),
                ],
              ),
      ),
    ],
  );
}
