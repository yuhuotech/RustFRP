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
import '../tunnels/tunnels_view.dart';
import 'enroll_dialog.dart';

class ClientsList extends ConsumerWidget {
  const ClientsList({super.key, this.narrow = false, this.showHeader = false});
  final bool narrow, showHeader;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final selected = ref.watch(selectedClientProvider);
    final clients = ref.watch(clientsProvider);
    return Column(
      children: [
        if (narrow || showHeader)
          PageHeader(
            title: '客户端',
            actions: [
              if (role.atLeast(Role.admin))
                if (showHeader)
                  LButton(
                    label: '接入新客户端',
                    icon: LucideIcons.plus,
                    variant: BtnVariant.solid,
                    onPressed: () => showEnrollDialog(context),
                  )
                else
                  LIconButton(
                    icon: LucideIcons.plus,
                    tooltip: '接入新客户端',
                    size: 32,
                    iconSize: 18,
                    onPressed: () => showEnrollDialog(context),
                  ),
            ],
          ),
        Expanded(
          child: AsyncView<List<ClientDevice>>(
            value: clients,
            onRetry: () => ref.invalidate(clientsProvider),
            builder: (list) => list.isEmpty
                ? EmptyState(
                    icon: LucideIcons.monitor,
                    title: '还没有客户端',
                    message: '接入第一台机器后，它会出现在这里。',
                    action: role.atLeast(Role.admin) && !showHeader
                        ? LButton(
                            label: '接入新客户端',
                            variant: BtnVariant.solid,
                            onPressed: () => showEnrollDialog(context),
                          )
                        : null,
                  )
                : ListView(
                    padding: EdgeInsets.all(showHeader ? 24 : 8),
                    children: [
                      for (final d in list)
                        Padding(
                          padding: const EdgeInsets.only(bottom: 2),
                          child: HoverRow(
                            key: Key('client-row-${d.id}'),
                            selected: !narrow && d.id == selected,
                            padding: const EdgeInsets.symmetric(
                              horizontal: 10,
                              vertical: 8,
                            ),
                            onTap: () {
                              ref
                                  .read(selectedClientProvider.notifier)
                                  .select(d.id);
                              if (narrow) {
                                pushDetail(context, ClientDetail(id: d.id));
                              }
                            },
                            child: Row(
                              children: [
                                DeviceGlyph(
                                  type: d.os,
                                  size: 36,
                                  online: d.online,
                                ),
                                const SizedBox(width: 10),
                                Expanded(
                                  child: Column(
                                    crossAxisAlignment:
                                        CrossAxisAlignment.start,
                                    children: [
                                      Text(
                                        d.name,
                                        style: Type.strong.copyWith(
                                          color: c.text1,
                                        ),
                                        overflow: TextOverflow.ellipsis,
                                      ),
                                      Text(
                                        '${d.disabled ? '已停用' : (d.online ? '${osLabel(d.os)} · 在线' : '${osLabel(d.os)} · ${relativeTime(d.lastSeenAt)}')}${d.tags.isEmpty ? '' : ' · ${d.tags.join(' / ')}'}',
                                        style: Type.caption.copyWith(
                                          color: c.text3,
                                        ),
                                        overflow: TextOverflow.ellipsis,
                                      ),
                                    ],
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ),
                    ],
                  ),
          ),
        ),
      ],
    );
  }
}

class ClientDetailHost extends ConsumerWidget {
  const ClientDetailHost({super.key});
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final id = ref.watch(selectedClientProvider);
    if (id == null) return const ClientsList(showHeader: true);
    return ClientDetail(id: id, returnToList: true);
  }
}

class ClientDetail extends ConsumerWidget {
  const ClientDetail({super.key, required this.id, this.returnToList = false});
  final String id;
  final bool returnToList;

  Future<void> _rename(
    BuildContext context,
    WidgetRef ref,
    ClientDevice d,
  ) async {
    final ctl = TextEditingController(text: d.name);
    final name = await showDialog<String>(
      context: context,
      builder: (ctx) => DialogShell(
        title: '重命名客户端',
        content: LTextField(
          key: const Key('rename-field'),
          controller: ctl,
          autofocus: true,
        ),
        actions: [
          LButton(label: '取消', onPressed: () => Navigator.pop(ctx)),
          LButton(
            label: '保存',
            variant: BtnVariant.solid,
            onPressed: () => Navigator.pop(ctx, ctl.text.trim()),
          ),
        ],
      ),
    );
    if (name == null || name.isEmpty || name == d.name || !context.mounted) {
      return;
    }
    await guarded(
      context,
      () => ref.read(actionsProvider).renameClient(d.id, name, '通过面板重命名客户端'),
    );
  }

  Future<void> _editTags(
    BuildContext context,
    WidgetRef ref,
    ClientDevice d,
  ) async {
    final ctl = TextEditingController(text: d.tags.join(', '));
    final text = await showDialog<String>(
      context: context,
      builder: (ctx) => DialogShell(
        title: '编辑标签',
        content: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              '用逗号分隔，最多 8 个，每个不超过 32 个字符。',
              style: Type.caption.copyWith(color: ctx.c.text3),
            ),
            const SizedBox(height: 8),
            LTextField(
              key: const Key('tags-field'),
              controller: ctl,
              autofocus: true,
              hint: 'prod, nas',
            ),
          ],
        ),
        actions: [
          LButton(label: '取消', onPressed: () => Navigator.pop(ctx)),
          LButton(
            key: const Key('tags-save'),
            label: '保存',
            variant: BtnVariant.solid,
            onPressed: () => Navigator.pop(ctx, ctl.text),
          ),
        ],
      ),
    );
    if (text == null || !context.mounted) return;
    final tags = [
      for (final t in text.split(RegExp(r'[,，]')))
        if (t.trim().isNotEmpty) t.trim(),
    ];
    await guarded(
      context,
      () => ref.read(actionsProvider).setClientTags(d.id, tags, '通过面板修改标签'),
    );
  }

  Future<void> _restart(
    BuildContext context,
    WidgetRef ref,
    ClientDevice d,
  ) async {
    final reason = await askReason(
      context,
      title: '重启 agent',
      message: '「${d.name}」上的 agent 会退出，由系统服务管理器重新拉起（需要以系统服务方式运行）。其隧道会短暂中断。',
      confirm: '重启',
      destructive: true,
    );
    if (reason == null || !context.mounted) return;
    final ok = await guarded(context, () async {
      await ref.read(actionsProvider).restartClient(d.id, reason);
      return true;
    });
    if (ok == true && context.mounted) showToast(context, '已发送重启指令');
  }

  Future<void> _toggleDisabled(
    BuildContext context,
    WidgetRef ref,
    ClientDevice d,
  ) async {
    final reason = await askReason(
      context,
      title: d.disabled ? '启用客户端' : '停用客户端',
      message: d.disabled
          ? '启用后「${d.name}」可以重新连接。'
          : '停用后「${d.name}」会立即断开，其隧道全部停止，且无法重新连接。',
      confirm: d.disabled ? '启用' : '停用',
      destructive: !d.disabled,
    );
    if (reason == null || !context.mounted) return;
    await guarded(
      context,
      () => ref
          .read(actionsProvider)
          .setClientDisabled(d.id, !d.disabled, reason),
    );
  }

  Future<void> _kick(
    BuildContext context,
    WidgetRef ref,
    ClientDevice d,
  ) async {
    final reason = await askReason(
      context,
      title: '强制下线',
      message: '「${d.name}」会被断开，随后自动重新连接。',
      confirm: '下线',
    );
    if (reason == null || !context.mounted) return;
    final ok = await guarded(context, () async {
      await ref.read(actionsProvider).kickClient(d.id, reason);
      return true;
    });
    if (ok == true && context.mounted) showToast(context, '已发送下线指令');
  }

  Future<void> _delete(
    BuildContext context,
    WidgetRef ref,
    ClientDevice d,
  ) async {
    final reason = await askReason(
      context,
      title: '删除客户端',
      message: '删除「${d.name}」及其全部隧道。此操作不可恢复，该设备需要重新接入。',
      confirm: '删除',
      destructive: true,
    );
    if (reason == null || !context.mounted) return;
    final ok = await guarded(context, () async {
      await ref.read(actionsProvider).deleteClient(d.id, reason);
      return true;
    });
    if (ok == true) {
      ref.read(selectedClientProvider.notifier).select(null);
      if (context.mounted && Navigator.of(context).canPop()) {
        Navigator.of(context).pop();
      }
    }
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final clients = ref.watch(clientsProvider);
    final tunnels = ref.watch(tunnelsProvider);
    final d = clients.asData?.value.where((x) => x.id == id).firstOrNull;
    if (d == null) {
      return Column(
        children: [
          const PageHeader(title: '客户端'),
          Expanded(
            child: clients.hasValue
                ? const EmptyState(
                    icon: LucideIcons.monitor,
                    title: '客户端不存在',
                    message: '它可能已被删除。',
                  )
                : AsyncView<List<ClientDevice>>(
                    value: clients,
                    builder: (_) => const SizedBox.shrink(),
                    onRetry: () => ref.invalidate(clientsProvider),
                  ),
          ),
        ],
      );
    }
    final canAdmin = role.atLeast(Role.admin);
    final mine =
        tunnels.asData?.value.where((t) => t.clientId == id).toList() ??
        const <Tunnel>[];
    return Column(
      children: [
        PageHeader(
          title: d.name,
          leading: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (returnToList) ...[
                LIconButton(
                  key: const Key('client-back'),
                  icon: LucideIcons.chevronLeft,
                  tooltip: '返回客户端列表',
                  size: 28,
                  onPressed: () =>
                      ref.read(selectedClientProvider.notifier).select(null),
                ),
                const SizedBox(width: 8),
              ],
              DeviceGlyph(type: d.os, size: 32, online: d.online),
            ],
          ),
          actions: [
            if (canAdmin) ...[
              LIconButton(
                icon: LucideIcons.pencil,
                tooltip: '重命名',
                size: 32,
                iconSize: 16,
                onPressed: () => _rename(context, ref, d),
              ),
              LIconButton(
                icon: d.disabled ? LucideIcons.play : LucideIcons.ban,
                tooltip: d.disabled ? '启用' : '停用',
                size: 32,
                iconSize: 16,
                onPressed: () => _toggleDisabled(context, ref, d),
              ),
              LIconButton(
                key: const Key('client-tags'),
                icon: LucideIcons.tags,
                tooltip: '标签',
                size: 32,
                iconSize: 16,
                onPressed: () => _editTags(context, ref, d),
              ),
              if (d.online) ...[
                LIconButton(
                  icon: LucideIcons.rotateCw,
                  tooltip: '重启 agent',
                  size: 32,
                  iconSize: 16,
                  onPressed: () => _restart(context, ref, d),
                ),
                LIconButton(
                  icon: LucideIcons.powerOff,
                  tooltip: '强制下线',
                  size: 32,
                  iconSize: 16,
                  onPressed: () => _kick(context, ref, d),
                ),
              ],
              LIconButton(
                icon: LucideIcons.trash2,
                tooltip: '删除客户端',
                size: 32,
                iconSize: 16,
                onPressed: () => _delete(context, ref, d),
              ),
            ],
          ],
        ),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.all(24),
            children: [
              Wrap(
                spacing: 6,
                runSpacing: 6,
                children: [
                  LBadge(
                    d.online ? '在线' : '离线',
                    bg: d.online ? c.successSoft : c.bgSubtle,
                    fg: d.online ? c.successText : c.text2,
                  ),
                  if (d.disabled)
                    LBadge('已停用', bg: c.dangerSoft, fg: c.dangerText),
                  for (final t in d.tags) LBadge(t),
                ],
              ),
              const SizedBox(height: 16),
              const SectionTitle('设备信息'),
              PanelCard(
                child: Column(
                  children: [
                    InfoRow('设备 ID', d.id, mono: true),
                    InfoRow('系统', '${osLabel(d.os)} ${d.arch}'),
                    InfoRow('Agent 版本', d.version.isEmpty ? '—' : d.version),
                    InfoRow('接入时间', fullTime(d.createdAt)),
                    InfoRow(
                      '最近在线',
                      d.online ? '现在' : relativeTime(d.lastSeenAt),
                    ),
                    InfoRow('配置版本', '${d.revision}'),
                  ],
                ),
              ),
              const SizedBox(height: 24),
              SectionTitle(
                '隧道（${mine.length}）',
                trailing: role.atLeast(Role.operator)
                    ? LButton(
                        key: const Key('client-new-tunnel'),
                        label: '新建隧道',
                        icon: LucideIcons.plus,
                        compact: true,
                        onPressed: () =>
                            showTunnelEditor(context, clientId: d.id),
                      )
                    : null,
              ),
              if (mine.isEmpty)
                PanelCard(
                  child: Text(
                    '这台客户端还没有隧道。',
                    style: Type.body.copyWith(color: c.text3),
                  ),
                )
              else
                for (final t in mine)
                  Padding(
                    padding: const EdgeInsets.only(bottom: 8),
                    child: TunnelCard(tunnel: t, showClient: false),
                  ),
            ],
          ),
        ),
      ],
    );
  }
}
