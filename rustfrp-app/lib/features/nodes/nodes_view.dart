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

/// List column: today there is exactly one node (the controller's built-in edge).
class NodesList extends ConsumerWidget {
  const NodesList({super.key, this.narrow = false});
  final bool narrow;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final node = ref.watch(nodeProvider);
    return Column(children: [
      if (narrow) const PageHeader(title: '节点'),
      Expanded(
        child: AsyncView<NodeInfo>(
          value: node,
          onRetry: () => ref.invalidate(nodeProvider),
          builder: (n) => ListView(padding: const EdgeInsets.all(8), children: [
            HoverRow(
              selected: !narrow,
              onTap: () => narrow ? pushDetail(context, const NodeDetail()) : null,
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
              child: Row(children: [
                Container(
                  width: 36,
                  height: 36,
                  decoration: BoxDecoration(color: c.bgCard, borderRadius: BorderRadius.circular(Radii.panel), border: Border.all(color: c.border)),
                  child: Icon(LucideIcons.server, size: 18, color: c.text1),
                ),
                const SizedBox(width: 10),
                Expanded(
                  child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                    Row(children: [
                      Flexible(child: Text('内置节点', style: Type.strong.copyWith(color: c.text1), overflow: TextOverflow.ellipsis)),
                      const SizedBox(width: 6),
                      StatusDot(color: c.success),
                    ]),
                    Text(n.publicHost.isEmpty ? '未配置公网地址' : hostPort(n.publicHost, n.edgePort),
                        style: Type.caption.copyWith(color: c.text3), overflow: TextOverflow.ellipsis),
                  ]),
                ),
              ]),
            ),
          ]),
        ),
      ),
    ]);
  }
}

class NodeDetail extends ConsumerWidget {
  const NodeDetail({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final node = ref.watch(nodeProvider);
    return Column(children: [
      const PageHeader(title: '内置节点', subtitle: 'builtin'),
      Expanded(
        child: AsyncView<NodeInfo>(
          value: node,
          onRetry: () => ref.invalidate(nodeProvider),
          builder: (n) => ListView(padding: const EdgeInsets.all(24), children: [
            const SectionTitle('状态'),
            PanelCard(
              child: Column(children: [
                InfoRow('状态', '运行中'),
                InfoRow('版本', n.version.isEmpty ? '—' : n.version),
                InfoRow('在线客户端', '${n.clientsOnline}'),
                InfoRow('公网地址', n.publicHost.isEmpty ? '未配置（设置 RUSTFRP_PUBLIC_HOST）' : n.publicHost, mono: n.publicHost.isNotEmpty),
                InfoRow('客户端接入端口', '${n.edgePort}', mono: true),
                InfoRow('可分配的远程端口', n.allowPortsLabel),
              ]),
            ),
            const SizedBox(height: 24),
            const SectionTitle('说明'),
            Text(
              '内置节点随控制器一起运行，客户端连接它的接入端口，并通过它暴露隧道。独立的多节点管理将在后续版本提供。',
              style: Type.body.copyWith(color: c.text2),
            ),
          ]),
        ),
      ),
    ]);
  }
}
