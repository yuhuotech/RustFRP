import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../shared/workspace_widgets.dart';
import '../../theme/tokens.dart';
import '../shell/shell.dart';
import '../tunnels/tunnels_view.dart';
import 'enroll_dialog.dart';
import 'visitors_section.dart';

class ClientsList extends ConsumerWidget {
  const ClientsList({super.key, this.narrow = false, this.showHeader = false});
  final bool narrow, showHeader;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final selected = ref.watch(selectedClientProvider);
    final clients = ref.watch(clientsProvider);
    final nodes = ref.watch(nodesProvider).asData?.value ?? const <NodeInfo>[];
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
                : showHeader
                ? ListView(
                    padding: const EdgeInsets.all(24),
                    children: [
                      PageIntro(
                        icon: LucideIcons.monitor,
                        title: '客户端设备',
                        description: '查看设备连接状态、所属服务端节点与 Agent 版本，点击设备管理隧道和访问者。',
                        color: c.directText,
                        badges: [
                          LBadge('共 ${list.length} 台'),
                          LBadge(
                            '${list.where((d) => d.online).length} 台在线',
                            bg: c.successSoft,
                            fg: c.successText,
                          ),
                        ],
                      ),
                      ResourceTable(
                        columns: const [
                          ('客户端 / 标签', 4),
                          ('状态', 2),
                          ('系统 / Agent', 3),
                          ('服务端节点', 3),
                          ('最近连接', 3),
                          ('操作', 2),
                        ],
                        rows: [
                          for (final d in list)
                            ResourceRow(
                              key: Key('client-row-${d.id}'),
                              onTap: () => ref
                                  .read(selectedClientProvider.notifier)
                                  .select(d.id),
                              cells: [
                                (
                                  Row(
                                    children: [
                                      DeviceGlyph(
                                        type: d.os,
                                        size: 32,
                                        online: d.online,
                                      ),
                                      const SizedBox(width: 10),
                                      Expanded(
                                        child: TableText(
                                          d.name,
                                          subtitle: d.tags.isEmpty
                                              ? d.id
                                              : d.tags.join(' / '),
                                        ),
                                      ),
                                    ],
                                  ),
                                  4,
                                ),
                                (
                                  Align(
                                    alignment: Alignment.centerLeft,
                                    child: LBadge(
                                      d.disabled
                                          ? '已停用'
                                          : (d.online ? '在线' : '离线'),
                                      bg: d.disabled
                                          ? c.bgSubtle
                                          : (d.online
                                                ? c.successSoft
                                                : c.warningSoft),
                                      fg: d.disabled
                                          ? c.text2
                                          : (d.online
                                                ? c.successText
                                                : c.warningText),
                                    ),
                                  ),
                                  2,
                                ),
                                (
                                  TableText(
                                    '${osLabel(d.os)} ${d.arch}',
                                    subtitle:
                                        'Agent ${d.version.isEmpty ? '—' : d.version}',
                                  ),
                                  3,
                                ),
                                (
                                  TableText(
                                    d.nodeId.isEmpty
                                        ? '内置节点'
                                        : nodes
                                                  .where(
                                                    (n) => n.id == d.nodeId,
                                                  )
                                                  .firstOrNull
                                                  ?.name ??
                                              d.nodeId,
                                    color: c.directText,
                                  ),
                                  3,
                                ),
                                (
                                  TableText(
                                    d.online
                                        ? '现在'
                                        : relativeTime(d.lastSeenAt),
                                    subtitle: d.lastIp.isEmpty
                                        ? '暂无登录 IP'
                                        : d.lastIp,
                                  ),
                                  3,
                                ),
                                (
                                  LButton(
                                    label: '查看详情',
                                    compact: true,
                                    onPressed: () => ref
                                        .read(selectedClientProvider.notifier)
                                        .select(d.id),
                                  ),
                                  2,
                                ),
                              ],
                            ),
                        ],
                      ),
                    ],
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

  Future<void> _editConnectionLimit(
    BuildContext context,
    WidgetRef ref,
    ClientDevice device,
  ) async {
    final text = await showDialog<String>(
      context: context,
      builder: (_) => _ClientTextDialog(
        fieldKey: 'client-max-connections',
        title: '客户端连接数上限',
        hint: '例如 500',
        initialValue: device.maxConnections == 0
            ? ''
            : '${device.maxConnections}',
        description: '所有隧道共用此上限；UDP 按活跃源地址计数，SUDP 按访问者工作流计数。留空或填 0 表示不限。',
        confirmLabel: '保存',
      ),
    );
    if (text == null || !context.mounted) return;
    final max = text.isEmpty ? 0 : int.tryParse(text);
    if (max == null || max < 0 || max > 1000000) {
      showToast(context, '连接上限须为 0 到 1000000 的整数');
      return;
    }
    await guarded(
      context,
      () => ref
          .read(actionsProvider)
          .setClientMaxConnections(device.id, max, '通过面板设置客户端连接数上限'),
    );
  }

  Future<void> _moveNode(
    BuildContext context,
    WidgetRef ref,
    ClientDevice device,
  ) async {
    final nodes = await guarded(context, () => ref.read(apiProvider).nodes());
    if (nodes == null || !context.mounted) return;
    final choices = nodes.where((node) => node.id != device.nodeId).toList();
    if (choices.isEmpty) {
      showToast(context, '没有可迁移的目标节点');
      return;
    }
    final nodeId = await showDialog<String>(
      context: context,
      builder: (dialogContext) => DialogShell(
        title: '迁移到其他节点',
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            for (final node in choices)
              ListTile(
                key: Key('move-node-${node.id.isEmpty ? 'builtin' : node.id}'),
                title: Text(node.name),
                subtitle: Text(
                  '${node.publicHost}:${node.edgePort} · ${node.online ? '在线' : '离线'}',
                ),
                onTap: () => Navigator.pop(dialogContext, node.id),
              ),
          ],
        ),
        actions: [
          LButton(label: '取消', onPressed: () => Navigator.pop(dialogContext)),
        ],
      ),
    );
    if (nodeId == null || !context.mounted) return;
    final reason = await askReason(
      context,
      title: '迁移客户端',
      message: '设备收到迁移指令后会自动连接目标 Edge；现有隧道会短暂中断。',
      confirm: '迁移',
    );
    if (reason == null || !context.mounted) return;
    final ok = await guarded(context, () async {
      await ref.read(actionsProvider).moveClient(device.id, nodeId, reason);
      return true;
    });
    if (ok == true && context.mounted) showToast(context, '已发送迁移指令');
  }

  Future<void> _updateAgent(
    BuildContext context,
    WidgetRef ref,
    ClientDevice device,
  ) async {
    final version = await showDialog<String>(
      context: context,
      builder: (_) => const _ClientTextDialog(
        fieldKey: 'agent-update-version',
        title: '更新 Agent',
        hint: 'Release tag，例如 v1.2.3',
        confirmLabel: '下一步',
      ),
    );
    if (version == null || version.isEmpty || !context.mounted) return;
    final reason = await askReason(
      context,
      title: '下发 Agent 更新',
      message: 'Agent 会从签名 Release 下载并校验新版本，替换后退出，由服务管理器重新拉起。',
      confirm: '下发更新',
    );
    if (reason == null || !context.mounted) return;
    final ok = await guarded(context, () async {
      await ref
          .read(actionsProvider)
          .updateClientAgent(device.id, version, reason);
      return true;
    });
    if (ok == true && context.mounted) showToast(context, '已发送更新指令');
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

  Future<void> _exportConfig(
    BuildContext context,
    WidgetRef ref,
    ClientDevice device,
  ) async {
    final text = await guarded(
      context,
      () => ref.read(actionsProvider).exportConfig(device.id),
    );
    if (text == null || !context.mounted) return;
    await showDialog<void>(
      context: context,
      builder: (dialogContext) => DialogShell(
        title: '导出 frpc 配置',
        maxWidth: 700,
        content: SelectableText(
          text,
          key: const Key('config-export-text'),
          style: Type.caption.copyWith(color: dialogContext.c.text2),
        ),
        actions: [
          LButton(
            label: '复制',
            onPressed: () async {
              await Clipboard.setData(ClipboardData(text: text));
              if (dialogContext.mounted) {
                showToast(dialogContext, '配置已复制');
              }
            },
          ),
          LButton(label: '关闭', onPressed: () => Navigator.pop(dialogContext)),
        ],
      ),
    );
  }

  Future<void> _importConfig(BuildContext context, ClientDevice device) async {
    final imported = await showDialog<bool>(
      context: context,
      builder: (_) => ConfigImportDialog(clientId: device.id),
    );
    if (imported == true && context.mounted) {
      showToast(context, '配置已导入');
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
            if (!isNarrow(context)) ...[
              if (role.atLeast(Role.operator))
                LIconButton(
                  key: const Key('client-config-import'),
                  icon: LucideIcons.fileUp,
                  tooltip: '导入 frpc 配置',
                  size: 32,
                  iconSize: 16,
                  onPressed: () => _importConfig(context, d),
                ),
              if (role.atLeast(Role.admin))
                LIconButton(
                  key: const Key('client-config-export'),
                  icon: LucideIcons.fileDown,
                  tooltip: '导出 frpc 配置',
                  size: 32,
                  iconSize: 16,
                  onPressed: () => _exportConfig(context, ref, d),
                ),
            ],
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
              if (!isNarrow(context)) ...[
                LIconButton(
                  key: const Key('client-connection-limit'),
                  icon: LucideIcons.gauge,
                  tooltip: '连接数上限',
                  size: 32,
                  iconSize: 16,
                  onPressed: () => _editConnectionLimit(context, ref, d),
                ),
                LIconButton(
                  key: const Key('client-migrate-node'),
                  icon: LucideIcons.moveRight,
                  tooltip: '迁移节点',
                  size: 32,
                  iconSize: 16,
                  onPressed: () => _moveNode(context, ref, d),
                ),
              ],
              if (d.online) ...[
                if (!isNarrow(context))
                  LIconButton(
                    key: const Key('client-update-agent'),
                    icon: LucideIcons.download,
                    tooltip: '更新 Agent',
                    size: 32,
                    iconSize: 16,
                    onPressed: () => _updateAgent(context, ref, d),
                  ),
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
              PageIntro(
                icon: DeviceGlyph.iconFor(d.os),
                title: '设备连接概况',
                description:
                    '${osLabel(d.os)} ${d.arch} · Agent ${d.version.isEmpty ? '版本未上报' : d.version} · 当前配置版本 ${d.revision}',
                color: c.directText,
                badges: [LBadge('共 ${mine.length} 条隧道')],
              ),
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
              DetailGrid(
                children: [
                  DetailPanel(
                    title: '设备信息',
                    icon: DeviceGlyph.iconFor(d.os),
                    color: c.directText,
                    child: Column(
                      children: [
                        InfoRow('设备 ID', d.id, mono: true),
                        InfoRow('系统', '${osLabel(d.os)} ${d.arch}'),
                        InfoRow(
                          'Agent 版本',
                          d.version.isEmpty ? '—' : d.version,
                        ),
                        InfoRow('配置版本', '${d.revision}'),
                        InfoRow(
                          '连接数上限',
                          d.maxConnections == 0 ? '不限' : '${d.maxConnections}',
                        ),
                        if (isNarrow(context) && canAdmin) ...[
                          const SizedBox(height: 12),
                          Wrap(
                            spacing: 8,
                            runSpacing: 8,
                            children: [
                              LButton(
                                key: const Key('client-connection-limit'),
                                label: '连接数上限',
                                compact: true,
                                onPressed: () =>
                                    _editConnectionLimit(context, ref, d),
                              ),
                              LButton(
                                key: const Key('client-migrate-node'),
                                label: '迁移节点',
                                compact: true,
                                onPressed: () => _moveNode(context, ref, d),
                              ),
                              if (d.online)
                                LButton(
                                  key: const Key('client-update-agent'),
                                  label: '更新 Agent',
                                  compact: true,
                                  onPressed: () =>
                                      _updateAgent(context, ref, d),
                                ),
                            ],
                          ),
                        ],
                      ],
                    ),
                  ),
                  DetailPanel(
                    title: '连接与接入',
                    icon: LucideIcons.network,
                    color: c.actionText,
                    child: Column(
                      children: [
                        InfoRow(
                          '所属节点',
                          d.nodeId.isEmpty ? '内置节点' : d.nodeId,
                          mono: d.nodeId.isNotEmpty,
                        ),
                        InfoRow('接入时间', fullTime(d.createdAt)),
                        InfoRow(
                          '最近在线',
                          d.online ? '现在' : relativeTime(d.lastSeenAt),
                        ),
                        InfoRow(
                          '最近 IP',
                          d.lastIp.isEmpty ? '—' : d.lastIp,
                          mono: true,
                        ),
                      ],
                    ),
                  ),
                ],
              ),
              if (isNarrow(context) && role.atLeast(Role.operator)) ...[
                const SizedBox(height: 12),
                Wrap(
                  spacing: 8,
                  children: [
                    LButton(
                      key: const Key('client-config-import'),
                      label: '导入 frpc 配置',
                      icon: LucideIcons.fileUp,
                      compact: true,
                      onPressed: () => _importConfig(context, d),
                    ),
                    if (role.atLeast(Role.admin))
                      LButton(
                        key: const Key('client-config-export'),
                        label: '导出 frpc 配置',
                        icon: LucideIcons.fileDown,
                        compact: true,
                        onPressed: () => _exportConfig(context, ref, d),
                      ),
                  ],
                ),
              ],
              const SizedBox(height: 24),
              SectionTitle(
                '隧道（${mine.length}）',
                icon: LucideIcons.waypoints,
                color: c.actionText,
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
              const SizedBox(height: 24),
              VisitorsSection(clientId: d.id),
            ],
          ),
        ),
      ],
    );
  }
}

class _ClientTextDialog extends StatefulWidget {
  const _ClientTextDialog({
    required this.fieldKey,
    required this.title,
    required this.hint,
    required this.confirmLabel,
    this.initialValue = '',
    this.description,
  });
  final String fieldKey, title, hint, confirmLabel, initialValue;
  final String? description;

  @override
  State<_ClientTextDialog> createState() => _ClientTextDialogState();
}

class _ClientTextDialogState extends State<_ClientTextDialog> {
  late final _controller = TextEditingController(text: widget.initialValue);

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => DialogShell(
    title: widget.title,
    content: Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (widget.description != null) ...[
          Text(
            widget.description!,
            style: Type.caption.copyWith(color: context.c.text2),
          ),
          const SizedBox(height: 8),
        ],
        LTextField(
          key: Key(widget.fieldKey),
          controller: _controller,
          hint: widget.hint,
          autofocus: true,
        ),
      ],
    ),
    actions: [
      LButton(label: '取消', onPressed: () => Navigator.pop(context)),
      LButton(
        label: widget.confirmLabel,
        variant: BtnVariant.solid,
        onPressed: () => Navigator.pop(context, _controller.text.trim()),
      ),
    ],
  );
}

class ConfigImportDialog extends ConsumerStatefulWidget {
  const ConfigImportDialog({super.key, required this.clientId});
  final String clientId;
  @override
  ConsumerState<ConfigImportDialog> createState() => _ConfigImportDialogState();
}

class _ConfigImportDialogState extends ConsumerState<ConfigImportDialog> {
  final _text = TextEditingController();
  final _reason = TextEditingController();
  Map<String, dynamic>? _preview;
  String? _previewText;
  String? _error;
  bool _loading = false;
  bool _acceptWarnings = false;

  @override
  void dispose() {
    _text.dispose();
    _reason.dispose();
    super.dispose();
  }

  Future<void> _previewConfig() async {
    final source = _text.text;
    if (source.trim().isEmpty || source.length > 1024 * 1024) {
      setState(() => _error = '请填写不超过 1 MiB 的 frpc 配置文本。');
      return;
    }
    setState(() {
      _loading = true;
      _error = null;
      _preview = null;
      _acceptWarnings = false;
    });
    try {
      final preview = await ref
          .read(actionsProvider)
          .previewConfigImport(widget.clientId, source);
      if (mounted) {
        setState(() {
          _preview = preview;
          _previewText = source;
          final warnings = preview['warnings'];
          _acceptWarnings = warnings is List && warnings.isEmpty;
        });
      }
    } on Object catch (e) {
      if (mounted) setState(() => _error = e.toString());
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  Future<void> _applyConfig() async {
    final preview = _preview;
    final digest = preview?['digest'];
    if (preview == null ||
        _previewText != _text.text ||
        digest is! String ||
        !_acceptWarnings ||
        _reason.text.trim().isEmpty) {
      setState(() => _error = '重新预览配置，并填写理由、确认兼容报告后再导入。');
      return;
    }
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      await ref
          .read(actionsProvider)
          .importConfig(
            widget.clientId,
            text: _text.text,
            digest: digest,
            reason: _reason.text.trim(),
          );
      if (mounted) {
        Navigator.pop(context, true);
      }
    } on Object catch (e) {
      if (mounted) setState(() => _error = e.toString());
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final preview = _preview;
    final changes = preview?['changes'] is List
        ? (preview!['changes'] as List).whereType<Map<String, dynamic>>()
        : const <Map<String, dynamic>>[];
    final warnings = preview?['warnings'] is List
        ? (preview!['warnings'] as List).whereType<String>()
        : const <String>[];
    final canApply =
        !_loading &&
        preview != null &&
        _previewText == _text.text &&
        _acceptWarnings &&
        _reason.text.trim().isNotEmpty;
    return DialogShell(
      title: '导入 frpc 配置',
      maxWidth: 720,
      content: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            '支持 TOML、YAML、JSON。导入只新增或更新配置中出现的隧道，不会删除其他隧道。',
            style: Type.caption.copyWith(color: c.text2),
          ),
          const SizedBox(height: 8),
          LTextField(
            key: const Key('config-import-text'),
            controller: _text,
            maxLines: 10,
            hint: '粘贴 frpc.toml / YAML / JSON',
            onChanged: (_) => setState(() {
              _preview = null;
              _previewText = null;
              _acceptWarnings = false;
            }),
          ),
          const SizedBox(height: 10),
          LButton(
            key: const Key('config-import-preview'),
            label: '预览差异',
            variant: BtnVariant.neutral,
            loading: _loading,
            onPressed: _loading ? null : _previewConfig,
          ),
          if (preview != null) ...[
            const SizedBox(height: 12),
            Text(
              '将应用 ${changes.length} 条隧道',
              style: Type.strong.copyWith(color: c.text1),
            ),
            for (final change in changes)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(
                  '${change['action']} · ${change['name']} · ${change['kind']}',
                  style: Type.caption.copyWith(color: c.text2),
                ),
              ),
            if (warnings.isNotEmpty) ...[
              const SizedBox(height: 10),
              Text('兼容提示', style: Type.strong.copyWith(color: c.warningText)),
              for (final warning in warnings)
                Padding(
                  padding: const EdgeInsets.only(top: 3),
                  child: Text(
                    warning,
                    style: Type.caption.copyWith(color: c.text2),
                  ),
                ),
              const SizedBox(height: 8),
              Row(
                children: [
                  LSwitch(
                    key: const Key('config-import-ack'),
                    value: _acceptWarnings,
                    onChanged: _loading
                        ? null
                        : (value) => setState(() => _acceptWarnings = value),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      '我已阅读提示，并接受未激活项保留在导出配置中。',
                      style: Type.caption.copyWith(color: c.text2),
                    ),
                  ),
                ],
              ),
            ],
            const SizedBox(height: 8),
            LTextField(
              key: const Key('config-import-reason'),
              controller: _reason,
              hint: '导入理由',
              onChanged: (_) => setState(() {}),
            ),
          ],
          if (_error != null) ...[
            const SizedBox(height: 8),
            Text(_error!, style: Type.caption.copyWith(color: c.dangerText)),
          ],
        ],
      ),
      actions: [
        LButton(
          label: '取消',
          onPressed: _loading ? null : () => Navigator.pop(context),
        ),
        LButton(
          key: const Key('config-import-apply'),
          label: '确认导入',
          variant: BtnVariant.solid,
          loading: _loading && preview != null,
          onPressed: canApply ? _applyConfig : null,
        ),
      ],
    );
  }
}
