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
            title: '节点',
            actions: [
              if (role.atLeast(Role.admin))
                if (showHeader)
                  LButton(
                    key: const Key('new-node'),
                    label: '接入新节点',
                    icon: LucideIcons.plus,
                    variant: BtnVariant.solid,
                    onPressed: () => showNodeEnrollDialog(context),
                  )
                else
                  LIconButton(
                    icon: LucideIcons.plus,
                    tooltip: '接入新节点',
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
                              borderRadius: BorderRadius.circular(Radii.panel),
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
                                  style: Type.caption.copyWith(color: c.text3),
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

class NodeDetail extends ConsumerWidget {
  const NodeDetail({super.key, required this.id, this.returnToList = false});
  final String id;
  final bool returnToList;

  Future<void> _rename(BuildContext context, WidgetRef ref, NodeInfo n) async {
    final ctl = TextEditingController(text: n.name);
    final name = await showDialog<String>(
      context: context,
      builder: (ctx) => DialogShell(
        title: '重命名节点',
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
      () => ref.read(actionsProvider).renameNode(n.id, name, '通过面板重命名节点'),
    );
  }

  Future<void> _toggle(BuildContext context, WidgetRef ref, NodeInfo n) async {
    final reason = await askReason(
      context,
      title: n.disabled ? '启用节点' : '停用节点',
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
      title: '删除节点',
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
          const PageHeader(title: '节点'),
          Expanded(
            child: nodes.hasValue
                ? const EmptyState(
                    icon: LucideIcons.server,
                    title: '节点不存在',
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
                  tooltip: '返回节点列表',
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
                tooltip: '删除节点',
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
              const SectionTitle('状态'),
              PanelCard(
                child: Column(
                  children: [
                    InfoRow('状态', n.builtin ? '运行中' : _nodeStatus(n)),
                    InfoRow('版本', n.version.isEmpty ? '—' : n.version),
                    if (!n.builtin && n.os.isNotEmpty)
                      InfoRow('系统', osLabel(n.os)),
                    InfoRow(
                      n.builtin ? '在线客户端' : '客户端',
                      '${n.builtin ? n.clientsOnline : n.clients}',
                    ),
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
                    InfoRow('可分配的远程端口', n.allowPortsLabel),
                    if (n.vhostHttpPort != null)
                      InfoRow('HTTP 虚拟主机端口', '${n.vhostHttpPort}', mono: true),
                    if (n.vhostHttpsPort != null)
                      InfoRow(
                        'HTTPS 虚拟主机端口',
                        '${n.vhostHttpsPort}',
                        mono: true,
                      ),
                    if (n.subdomainHost.isNotEmpty)
                      InfoRow('子域名后缀', n.subdomainHost, mono: true),
                    if (n.tlsFingerprint.isNotEmpty)
                      InfoRow('证书指纹', n.tlsFingerprint, mono: true),
                  ],
                ),
              ),
              const SizedBox(height: 24),
              const SectionTitle('说明'),
              Text(
                n.builtin
                    ? '内置节点随控制器一起运行，控制器退出时它也会停止。需要「控制器离线隧道不中断」时，请接入独立节点。'
                    : '独立节点是单独运行的 Edge 进程：控制器离线时，已连接的客户端和隧道继续工作，节点会缓存设备与配置并在控制器恢复后自动对账。',
                style: Type.body.copyWith(color: c.text2),
              ),
            ],
          ),
        ),
      ],
    );
  }
}
