import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';
import '../clients/enroll_dialog.dart';
import '../shell/shell.dart';

Color _nodeColor(BuildContext context, NodeInfo n) {
  final c = context.c;
  if (n.disabled) return c.text3;
  return n.online ? c.success : c.danger;
}

String _nodeStatus(NodeInfo n) => n.disabled
    ? '已停用'
    : n.online
    ? '在线'
    : '控制连接断开 · ${relativeTime(n.lastSeenAt)}';

String _nodeCapabilities(NodeInfo n) {
  final items = <String>[];
  if (n.version.isNotEmpty) items.add('v${n.version}');
  items.add('远程端口 ${n.allowPortsLabel}');
  if (n.vhostHttpPort != null) items.add('HTTP ${n.vhostHttpPort}');
  if (n.vhostHttpsPort != null) items.add('HTTPS ${n.vhostHttpsPort}');
  if (n.subdomainHost.isNotEmpty) items.add('子域名');
  return items.isEmpty ? 'TCP / UDP' : items.join(' · ');
}

Widget _nodeCell(
  BuildContext context,
  String text, {
  int flex = 1,
  bool mono = false,
  Color? color,
}) => Expanded(
  flex: flex,
  child: Tooltip(
    message: text,
    child: Text(
      text,
      style: Type.body.copyWith(
        color: color ?? context.c.text1,
        fontFamily: mono ? 'monospace' : null,
        fontFamilyFallback: mono ? Type.monoFallback : null,
      ),
      maxLines: 2,
      overflow: TextOverflow.ellipsis,
    ),
  ),
);

Widget _nodeTableRow(
  BuildContext context,
  NodeInfo n, {
  required bool selected,
  required VoidCallback onTap,
  required VoidCallback onView,
}) {
  final c = context.c;
  final status = n.builtin ? '运行中' : _nodeStatus(n);
  return HoverRow(
    key: Key('node-row-${n.id.isEmpty ? 'builtin' : n.id}'),
    selected: selected,
    onTap: onTap,
    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
    child: Row(
      children: [
        Expanded(
          flex: 3,
          child: Row(
            children: [
              Container(
                width: 32,
                height: 32,
                decoration: BoxDecoration(
                  color: n.builtin ? c.actionSoft : c.directSoft,
                  borderRadius: BorderRadius.circular(Radii.control),
                  border: Border.all(
                    color: n.builtin
                        ? c.action.withValues(alpha: 0.25)
                        : c.direct.withValues(alpha: 0.25),
                  ),
                ),
                child: Icon(
                  LucideIcons.server,
                  size: 16,
                  color: n.builtin ? c.actionText : c.directText,
                ),
              ),
              const SizedBox(width: 10),
              Expanded(
                child: Tooltip(
                  message: n.builtin ? '内置节点' : n.name,
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        n.builtin ? '内置节点' : n.name,
                        style: Type.strong.copyWith(color: c.text1),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                      LBadge(
                        n.builtin ? '内置服务端' : '独立服务端',
                        bg: n.builtin ? c.actionSoft : c.directSoft,
                        fg: n.builtin ? c.actionText : c.directText,
                      ),
                    ],
                  ),
                ),
              ),
            ],
          ),
        ),
        const SizedBox(width: 12),
        Expanded(
          flex: 2,
          child: Tooltip(message: status, child: _nodeStatusBadge(context, n)),
        ),
        const SizedBox(width: 12),
        _nodeCell(
          context,
          '${n.builtin ? n.clientsOnline : n.clients} 台',
          flex: 1,
        ),
        const SizedBox(width: 12),
        _nodeCell(
          context,
          n.publicHost.isEmpty ? '未配置公网地址' : hostPort(n.publicHost, n.edgePort),
          flex: 3,
          mono: true,
          color: n.publicHost.isEmpty ? c.text3 : c.directText,
        ),
        const SizedBox(width: 12),
        _nodeCell(context, _nodeCapabilities(n), flex: 3, color: c.actionText),
        const SizedBox(width: 8),
        SizedBox(
          width: 36,
          child: LIconButton(
            icon: LucideIcons.chevronRight,
            tooltip: '查看详情',
            size: 32,
            iconSize: 16,
            onPressed: onView,
          ),
        ),
      ],
    ),
  );
}

class _NodeTable extends StatelessWidget {
  const _NodeTable({
    required this.nodes,
    required this.selected,
    required this.onSelect,
  });
  final List<NodeInfo> nodes;
  final String? selected;
  final ValueChanged<String> onSelect;

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return PanelCard(
      padding: EdgeInsets.zero,
      child: Column(
        children: [
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 9),
            decoration: BoxDecoration(
              color: c.bgSubtle,
              border: Border(bottom: BorderSide(color: c.border)),
              borderRadius: const BorderRadius.vertical(
                top: Radius.circular(Radii.panel),
              ),
            ),
            child: Row(
              children: [
                _nodeCell(context, '服务端节点', flex: 3, color: c.text3),
                const SizedBox(width: 12),
                _nodeCell(context, '状态', flex: 2, color: c.text3),
                const SizedBox(width: 12),
                _nodeCell(context, '客户端', color: c.text3),
                const SizedBox(width: 12),
                _nodeCell(context, '公网接入地址', flex: 3, color: c.text3),
                const SizedBox(width: 12),
                _nodeCell(context, '服务能力', flex: 3, color: c.text3),
                const SizedBox(width: 44),
              ],
            ),
          ),
          for (var i = 0; i < nodes.length; i++) ...[
            if (i > 0) Divider(height: 1, color: c.border),
            _nodeTableRow(
              context,
              nodes[i],
              selected: nodes[i].id == selected,
              onTap: () => onSelect(nodes[i].id),
              onView: () => onSelect(nodes[i].id),
            ),
          ],
        ],
      ),
    );
  }
}

class NodesList extends ConsumerWidget {
  const NodesList({super.key, this.narrow = false, this.showHeader = false});
  final bool narrow, showHeader;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final selected = ref.watch(selectedNodeProvider);
    final nodes = ref.watch(nodesProvider);
    return Column(
      children: [
        if (narrow || showHeader)
          PageHeader(
            title: '节点管理',
            actions: [
              if (role.atLeast(Role.admin))
                if (showHeader)
                  LButton(
                    key: const Key('new-node'),
                    label: '接入服务端节点',
                    icon: LucideIcons.plus,
                    variant: BtnVariant.solid,
                    onPressed: () => showNodeEnrollDialog(context),
                  )
                else
                  LIconButton(
                    icon: LucideIcons.plus,
                    tooltip: '接入服务端节点',
                    size: 32,
                    iconSize: 18,
                    onPressed: () => showNodeEnrollDialog(context),
                  ),
            ],
          ),
        Expanded(
          child: AsyncView<List<NodeInfo>>(
            value: nodes,
            onRetry: () => ref.invalidate(nodesProvider),
            builder: (list) => ListView(
              padding: EdgeInsets.all(showHeader ? 24 : 8),
              children: [
                if (showHeader) ...[
                  Text(
                    '服务端节点承载客户端连接和隧道流量。内置节点随 Controller 运行，独立服务端节点可单独运行。',
                    style: Type.body.copyWith(color: c.text2),
                  ),
                  const SizedBox(height: 16),
                  _NodeTable(
                    nodes: list,
                    selected: selected,
                    onSelect: (id) =>
                        ref.read(selectedNodeProvider.notifier).select(id),
                  ),
                ] else
                  for (final n in list)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 2),
                      child: HoverRow(
                        key: Key('node-row-${n.id.isEmpty ? 'builtin' : n.id}'),
                        selected: !narrow && n.id == selected,
                        onTap: () {
                          ref.read(selectedNodeProvider.notifier).select(n.id);
                          if (narrow) {
                            pushDetail(context, NodeDetail(id: n.id));
                          }
                        },
                        padding: const EdgeInsets.symmetric(
                          horizontal: 10,
                          vertical: 8,
                        ),
                        child: Row(
                          children: [
                            Container(
                              width: 36,
                              height: 36,
                              decoration: BoxDecoration(
                                color: c.bgCard,
                                borderRadius: BorderRadius.circular(
                                  Radii.panel,
                                ),
                                border: Border.all(color: c.border),
                              ),
                              child: Icon(
                                LucideIcons.server,
                                size: 18,
                                color: c.text1,
                              ),
                            ),
                            const SizedBox(width: 10),
                            Expanded(
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Row(
                                    children: [
                                      Flexible(
                                        child: Text(
                                          n.builtin ? '内置节点' : n.name,
                                          style: Type.strong.copyWith(
                                            color: c.text1,
                                          ),
                                          overflow: TextOverflow.ellipsis,
                                        ),
                                      ),
                                      const SizedBox(width: 6),
                                      StatusDot(color: _nodeColor(context, n)),
                                    ],
                                  ),
                                  Text(
                                    '${n.publicHost.isEmpty ? '未配置公网地址' : hostPort(n.publicHost, n.edgePort)} · ${n.clients} 台客户端',
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

/// 宽屏内容区：未选中显示节点列表，选中后显示详情（与客户端区一致）。
class NodeDetailHost extends ConsumerWidget {
  const NodeDetailHost({super.key});
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final id = ref.watch(selectedNodeProvider);
    if (id == null) return const NodesList(showHeader: true);
    return NodeDetail(id: id, returnToList: true);
  }
}

class _NodeDetailCard extends StatelessWidget {
  const _NodeDetailCard({
    required this.icon,
    required this.title,
    required this.child,
  });
  final IconData icon;
  final String title;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return PanelCard(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Container(
                width: 28,
                height: 28,
                decoration: BoxDecoration(
                  color: c.actionSoft,
                  borderRadius: BorderRadius.circular(Radii.control),
                ),
                child: Icon(icon, size: 15, color: c.actionText),
              ),
              const SizedBox(width: 10),
              Text(title, style: Type.strong.copyWith(color: c.text1)),
            ],
          ),
          const SizedBox(height: 12),
          child,
        ],
      ),
    );
  }
}

Widget _nodeStatusBadge(BuildContext context, NodeInfo n) {
  final c = context.c;
  final active = n.builtin || n.online;
  final label = n.builtin
      ? '运行中'
      : (n.disabled ? '已停用' : (n.online ? '在线' : '离线'));
  return Row(
    mainAxisSize: MainAxisSize.min,
    children: [
      StatusDot(color: n.disabled ? c.text3 : (active ? c.success : c.danger)),
      const SizedBox(width: 6),
      LBadge(
        label,
        bg: n.disabled ? c.bgSubtle : (active ? c.successSoft : c.dangerSoft),
        fg: n.disabled ? c.text2 : (active ? c.successText : c.dangerText),
      ),
    ],
  );
}

Widget _nodeCapabilityBadges(BuildContext context, NodeInfo n) {
  final c = context.c;
  final badges = <Widget>[
    const LBadge('TCP'),
    const LBadge('UDP'),
    LBadge('远程端口 ${n.allowPortsLabel}'),
  ];
  if (n.vhostHttpPort != null) {
    badges.add(LBadge('HTTP :${n.vhostHttpPort}'));
  }
  if (n.vhostHttpsPort != null) {
    badges.add(LBadge('HTTPS :${n.vhostHttpsPort}'));
  }
  if (n.tcpmuxHttpConnectPort != null) {
    badges.add(LBadge('TCPMUX :${n.tcpmuxHttpConnectPort}'));
  }
  if (n.subdomainHost.isNotEmpty) {
    badges.add(
      LBadge('子域名 ${n.subdomainHost}', bg: c.directSoft, fg: c.directText),
    );
  }
  return Wrap(spacing: 6, runSpacing: 6, children: badges);
}

class NodeDetail extends ConsumerWidget {
  const NodeDetail({super.key, required this.id, this.returnToList = false});
  final String id;
  final bool returnToList;

  Future<void> _rename(BuildContext context, WidgetRef ref, NodeInfo n) async {
    final ctl = TextEditingController(text: n.name);
    final name = await showDialog<String>(
      context: context,
      builder: (ctx) => DialogShell(
        title: '重命名服务端节点',
        content: LTextField(
          key: const Key('node-rename-field'),
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
    if (name == null || name.isEmpty || name == n.name || !context.mounted) {
      return;
    }
    await guarded(
      context,
      () => ref.read(actionsProvider).renameNode(n.id, name, '通过面板重命名服务端节点'),
    );
  }

  Future<void> _toggle(BuildContext context, WidgetRef ref, NodeInfo n) async {
    final reason = await askReason(
      context,
      title: n.disabled ? '启用服务端节点' : '停用服务端节点',
      message: n.disabled
          ? '启用后「${n.name}」可以重新连接 Controller。'
          : '停用后「${n.name}」与 Controller 的连接会被切断且无法重连；它已缓存的设备仍可使用，直到被重启。',
      confirm: n.disabled ? '启用' : '停用',
      destructive: !n.disabled,
    );
    if (reason == null || !context.mounted) return;
    await guarded(
      context,
      () =>
          ref.read(actionsProvider).setNodeDisabled(n.id, !n.disabled, reason),
    );
  }

  Future<void> _delete(BuildContext context, WidgetRef ref, NodeInfo n) async {
    final reason = await askReason(
      context,
      title: '删除服务端节点',
      message: '删除「${n.name}」。仍有客户端使用它时无法删除。',
      confirm: '删除',
      destructive: true,
    );
    if (reason == null || !context.mounted) return;
    final ok = await guarded(context, () async {
      await ref.read(actionsProvider).deleteNode(n.id, reason);
      return true;
    });
    if (ok == true) {
      ref.read(selectedNodeProvider.notifier).select(null);
      if (context.mounted && Navigator.of(context).canPop()) {
        Navigator.of(context).pop();
      }
    }
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final nodes = ref.watch(nodesProvider);
    final n = nodes.asData?.value.where((x) => x.id == id).firstOrNull;
    if (n == null) {
      return Column(
        children: [
          const PageHeader(title: '节点管理'),
          Expanded(
            child: nodes.hasValue
                ? const EmptyState(
                    icon: LucideIcons.server,
                    title: '服务端节点不存在',
                    message: '它可能已被删除。',
                  )
                : AsyncView<List<NodeInfo>>(
                    value: nodes,
                    builder: (_) => const SizedBox.shrink(),
                    onRetry: () => ref.invalidate(nodesProvider),
                  ),
          ),
        ],
      );
    }
    final canAdmin = role.atLeast(Role.admin) && !n.builtin;
    return Column(
      children: [
        PageHeader(
          title: n.builtin ? '内置节点' : n.name,
          subtitle: n.builtin ? 'builtin' : n.id,
          leading: returnToList
              ? LIconButton(
                  key: const Key('node-back'),
                  icon: LucideIcons.chevronLeft,
                  tooltip: '返回服务端节点列表',
                  size: 28,
                  onPressed: () =>
                      ref.read(selectedNodeProvider.notifier).select(null),
                )
              : null,
          actions: [
            if (canAdmin) ...[
              LIconButton(
                icon: LucideIcons.pencil,
                tooltip: '重命名',
                size: 32,
                iconSize: 16,
                onPressed: () => _rename(context, ref, n),
              ),
              LIconButton(
                icon: n.disabled ? LucideIcons.play : LucideIcons.ban,
                tooltip: n.disabled ? '启用' : '停用',
                size: 32,
                iconSize: 16,
                onPressed: () => _toggle(context, ref, n),
              ),
              LIconButton(
                icon: LucideIcons.trash2,
                tooltip: '删除服务端节点',
                size: 32,
                iconSize: 16,
                onPressed: () => _delete(context, ref, n),
              ),
            ],
          ],
        ),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.all(24),
            children: [
              PanelCard(
                padding: const EdgeInsets.all(20),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Container(
                      width: 52,
                      height: 52,
                      decoration: BoxDecoration(
                        color: c.actionSoft,
                        borderRadius: BorderRadius.circular(Radii.panel),
                        border: Border.all(
                          color: c.action.withValues(alpha: 0.25),
                        ),
                      ),
                      child: Icon(
                        LucideIcons.server,
                        size: 25,
                        color: c.actionText,
                      ),
                    ),
                    const SizedBox(width: 14),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Wrap(
                            spacing: 8,
                            runSpacing: 6,
                            crossAxisAlignment: WrapCrossAlignment.center,
                            children: [
                              Text(
                                n.builtin ? '内置节点' : n.name,
                                style: Type.title.copyWith(color: c.text1),
                              ),
                              LBadge(n.builtin ? '内置' : '独立'),
                              _nodeStatusBadge(context, n),
                            ],
                          ),
                          const SizedBox(height: 6),
                          Text(
                            n.builtin
                                ? 'Controller 内置 Edge，随控制器一起提供客户端接入和隧道转发。'
                                : '独立运行的 Edge 服务端节点，Controller 离线时仍可维持已建立的隧道。',
                            style: Type.body.copyWith(color: c.text2),
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: 16),
                    Column(
                      crossAxisAlignment: CrossAxisAlignment.end,
                      children: [
                        Text(
                          '${n.builtin ? n.clientsOnline : n.clients}',
                          style: Type.metric.copyWith(color: c.text1),
                        ),
                        Text(
                          '在线客户端',
                          style: Type.caption.copyWith(color: c.text3),
                        ),
                      ],
                    ),
                  ],
                ),
              ),
              const SizedBox(height: 16),
              LayoutBuilder(
                builder: (context, box) {
                  final columns = box.maxWidth >= 900 ? 2 : 1;
                  final width = (box.maxWidth - (columns - 1) * 12) / columns;
                  return Wrap(
                    spacing: 12,
                    runSpacing: 12,
                    children: [
                      SizedBox(
                        width: width,
                        child: _NodeDetailCard(
                          icon: LucideIcons.activity,
                          title: '运行状态',
                          child: Column(
                            children: [
                              InfoRow(
                                '当前状态',
                                n.builtin ? '运行中' : _nodeStatus(n),
                              ),
                              InfoRow(
                                'Edge 版本',
                                n.version.isEmpty ? '—' : n.version,
                              ),
                              if (!n.builtin && n.os.isNotEmpty)
                                InfoRow('运行系统', osLabel(n.os)),
                              InfoRow(
                                '已接入客户端',
                                '${n.builtin ? n.clientsOnline : n.clients} 台',
                              ),
                            ],
                          ),
                        ),
                      ),
                      SizedBox(
                        width: width,
                        child: _NodeDetailCard(
                          icon: LucideIcons.globe2,
                          title: '网络与接入',
                          child: Column(
                            children: [
                              InfoRow(
                                '公网地址',
                                n.publicHost.isEmpty
                                    ? (n.builtin
                                          ? '未配置（设置 RUSTFRP_PUBLIC_HOST）'
                                          : '节点尚未上报')
                                    : n.publicHost,
                                mono: n.publicHost.isNotEmpty,
                              ),
                              InfoRow('客户端接入端口', '${n.edgePort}', mono: true),
                              InfoRow('远程端口范围', n.allowPortsLabel),
                            ],
                          ),
                        ),
                      ),
                      SizedBox(
                        width: width,
                        child: _NodeDetailCard(
                          icon: LucideIcons.waypoints,
                          title: '服务能力',
                          child: _nodeCapabilityBadges(context, n),
                        ),
                      ),
                      SizedBox(
                        width: width,
                        child: _NodeDetailCard(
                          icon: LucideIcons.fingerprint,
                          title: '身份与安全',
                          child: Column(
                            children: [
                              InfoRow(
                                '节点 ID',
                                n.builtin ? 'builtin' : n.id,
                                mono: true,
                              ),
                              if (n.subdomainHost.isNotEmpty)
                                InfoRow('子域名后缀', n.subdomainHost, mono: true),
                              InfoRow(
                                '证书指纹',
                                n.tlsFingerprint.isEmpty
                                    ? '—'
                                    : n.tlsFingerprint,
                                mono: n.tlsFingerprint.isNotEmpty,
                              ),
                            ],
                          ),
                        ),
                      ),
                    ],
                  );
                },
              ),
              const SizedBox(height: 16),
              _NodeDetailCard(
                icon: LucideIcons.info,
                title: '节点说明',
                child: Text(
                  n.builtin
                      ? '内置节点随控制器一起运行，控制器退出时它也会停止。需要控制器离线后隧道仍不中断时，请接入独立服务端节点。'
                      : '独立服务端节点是单独运行的 Edge 进程：控制器离线时，已连接的客户端和隧道继续工作；节点会缓存设备与配置，并在控制器恢复后自动对账。',
                  style: Type.body.copyWith(color: c.text2),
                ),
              ),
              const SizedBox(height: 16),
              _NodeDetailCard(
                icon: canAdmin ? LucideIcons.settings2 : LucideIcons.circleHelp,
                title: '管理操作',
                child: canAdmin
                    ? Wrap(
                        spacing: 8,
                        runSpacing: 8,
                        children: [
                          LButton(
                            label: '重命名',
                            icon: LucideIcons.pencil,
                            compact: true,
                            onPressed: () => _rename(context, ref, n),
                          ),
                          LButton(
                            label: n.disabled ? '启用节点' : '停用节点',
                            icon: n.disabled
                                ? LucideIcons.play
                                : LucideIcons.ban,
                            compact: true,
                            onPressed: () => _toggle(context, ref, n),
                          ),
                          LButton(
                            label: '删除节点',
                            icon: LucideIcons.trash2,
                            variant: BtnVariant.destructive,
                            compact: true,
                            onPressed: () => _delete(context, ref, n),
                          ),
                        ],
                      )
                    : Text(
                        n.builtin
                            ? '内置节点由 Controller 管理，不支持重命名、停用或删除。'
                            : '当前账号没有节点管理权限，可查看运行状态和接入配置。',
                        style: Type.body.copyWith(color: c.text2),
                      ),
              ),
            ],
          ),
        ),
      ],
    );
  }
}
