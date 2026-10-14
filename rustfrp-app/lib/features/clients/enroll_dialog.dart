import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';

Future<void> showEnrollDialog(BuildContext context) =>
    showDialog<void>(context: context, builder: (_) => const EnrollDialog());

/// 接入服务端节点（独立 Edge 进程）。
Future<void> showNodeEnrollDialog(BuildContext context) => showDialog<void>(
  context: context,
  builder: (_) => const EnrollDialog(forNode: true),
);

/// Wizard: name it → get the one-time install command → wait for the device (or node) to come online.
class EnrollDialog extends ConsumerStatefulWidget {
  const EnrollDialog({super.key, this.forNode = false});

  /// true = 接入服务端节点；false = 接入新客户端。
  final bool forNode;
  @override
  ConsumerState<EnrollDialog> createState() => _EnrollDialogState();
}

class _EnrollDialogState extends ConsumerState<EnrollDialog> {
  final _name = TextEditingController();
  int _ttl = 60;
  EnrollResult? _result;
  bool _busy = false;
  String _platform = 'linux_macos';

  /// 客户端令牌指定的节点（空串 = 内置节点）。
  String _nodeId = '';

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  /// 选中的节点；内置节点关闭或选项失效时落到第一个可用的外置节点。
  String _effectiveNodeId() {
    final usable = (ref.read(nodesProvider).asData?.value ?? const <NodeInfo>[])
        .where((n) => !n.disabled);
    if (usable.any((n) => n.id == _nodeId) || usable.isEmpty) return _nodeId;
    return usable.first.id;
  }

  Future<void> _create() async {
    setState(() => _busy = true);
    final r = await guarded(
      context,
      () => ref
          .read(actionsProvider)
          .createToken(
            _name.text.trim(),
            _ttl,
            widget.forNode
                ? '通过面板接入服务端节点「${_name.text.trim()}」'
                : '通过面板接入新客户端「${_name.text.trim()}」',
            kind: widget.forNode ? 'node' : 'client',
            nodeId: widget.forNode ? '' : _effectiveNodeId(),
          ),
    );
    if (!mounted) return;
    setState(() {
      _busy = false;
      _result = r;
      if (r != null && !r.commands.containsKey(_platform)) _platform = 'direct';
    });
  }

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final r = _result;
    final nodes = ref.watch(nodesProvider).asData?.value ?? const <NodeInfo>[];
    if (r == null) {
      return DialogShell(
        title: widget.forNode ? '接入服务端节点' : '接入新客户端',
        content: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              widget.forNode
                  ? '给这个服务端节点起个名字，然后在它的服务器上执行生成的命令。它是单独运行的 Edge：控制器离线时，已连接的客户端和隧道继续工作。'
                  : '给这台机器起个名字，然后复制生成的命令到它的终端里执行。命令里的令牌只能使用一次。',
              style: Type.body.copyWith(color: c.text2),
            ),
            const SizedBox(height: 14),
            LTextField(
              key: const Key('enroll-name'),
              controller: _name,
              hint: widget.forNode ? '例如：香港节点' : '例如：家里的 NAS',
              autofocus: true,
              onChanged: (_) => setState(() {}),
            ),
            if (!widget.forNode &&
                nodes.where((n) => !n.builtin).isNotEmpty) ...[
              const SizedBox(height: 12),
              Text('接入到节点', style: Type.caption.copyWith(color: c.text3)),
              const SizedBox(height: 4),
              LSelect<String>(
                key: const Key('enroll-node'),
                value: _effectiveNodeId(),
                options: [
                  for (final n in nodes)
                    if (!n.disabled)
                      LSelectOption(n.id, n.builtin ? '内置节点' : n.name),
                ],
                onChanged: (v) => setState(() => _nodeId = v ?? ''),
              ),
            ],
            const SizedBox(height: 12),
            Text('令牌有效期', style: Type.caption.copyWith(color: c.text3)),
            const SizedBox(height: 4),
            LSelect<int>(
              value: _ttl,
              options: const [
                LSelectOption(15, '15 分钟'),
                LSelectOption(60, '1 小时'),
                LSelectOption(1440, '24 小时'),
                LSelectOption(10080, '7 天'),
              ],
              onChanged: (v) => setState(() => _ttl = v ?? 60),
            ),
          ],
        ),
        actions: [
          LButton(label: '取消', onPressed: () => Navigator.pop(context)),
          LButton(
            key: const Key('enroll-create'),
            label: '生成命令',
            variant: BtnVariant.solid,
            loading: _busy,
            onPressed: _name.text.trim().isEmpty ? null : _create,
          ),
        ],
      );
    }
    final cmds = r.commands;
    final platforms = [
      if (!widget.forNode && cmds.containsKey('linux_macos'))
        const LSelectOption('linux_macos', 'Linux / macOS（一行安装）'),
      if (!widget.forNode && cmds.containsKey('windows'))
        const LSelectOption('windows', 'Windows（PowerShell）'),
      const LSelectOption('direct', '已安装 agent，直接运行'),
    ];
    final clients =
        ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    final token = ref
        .watch(tokensProvider)
        .asData
        ?.value
        .where((t) => t.id == r.id)
        .firstOrNull;
    final joined = widget.forNode
        ? null
        : clients
              .where(
                (d) =>
                    d.id == token?.clientId &&
                    d.online &&
                    token?.usedAt != null,
              )
              .firstOrNull;
    // 节点令牌把节点 ID 记在 clientId 字段里。
    final joinedNode = widget.forNode
        ? nodes
              .where(
                (n) =>
                    n.id == token?.clientId &&
                    n.online &&
                    token?.usedAt != null,
              )
              .firstOrNull
        : null;
    final done = joined != null || joinedNode != null;
    return DialogShell(
      title: !done ? '在目标机器上执行' : '接入成功',
      maxWidth: 560,
      content: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          if (!done) ...[
            LSelect<String>(
              value: _platform,
              options: platforms,
              onChanged: (v) => setState(() => _platform = v ?? 'direct'),
            ),
            const SizedBox(height: 12),
            CopyField(
              key: const Key('enroll-command'),
              text: cmds[_platform] ?? cmds['direct'] ?? '',
            ),
            const SizedBox(height: 12),
            Text(
              widget.forNode
                  ? '把 <this-node-public-host> 换成客户端访问这个节点所用的域名或 IP。令牌将在 ${remaining(r.expiresAt)} 后失效，用过一次即作废。'
                  : '令牌将在 ${remaining(r.expiresAt)} 后失效，用过一次即作废。',
              style: Type.caption.copyWith(color: c.text3),
            ),
            const SizedBox(height: 14),
            Row(
              children: [
                const SizedBox(
                  width: 14,
                  height: 14,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
                const SizedBox(width: 8),
                Text(
                  widget.forNode ? '等待节点上线…' : '等待设备上线…',
                  style: Type.body.copyWith(color: c.text2),
                ),
              ],
            ),
          ] else if (joinedNode != null)
            Row(
              children: [
                Icon(LucideIcons.server, size: 32, color: c.text1),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        joinedNode.name,
                        key: const Key('enroll-joined-node'),
                        style: Type.strong.copyWith(color: c.text1),
                      ),
                      Text(
                        '${joinedNode.publicHost} · agent ${joinedNode.version}',
                        style: Type.caption.copyWith(color: c.text3),
                      ),
                    ],
                  ),
                ),
              ],
            )
          else if (joined != null)
            Row(
              children: [
                DeviceGlyph(type: joined.os, size: 40, online: joined.online),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        joined.name,
                        key: const Key('enroll-joined'),
                        style: Type.strong.copyWith(color: c.text1),
                      ),
                      Text(
                        '${osLabel(joined.os)} · ${joined.arch} · agent ${joined.version}',
                        style: Type.caption.copyWith(color: c.text3),
                      ),
                    ],
                  ),
                ),
              ],
            ),
        ],
      ),
      actions: [
        if (joinedNode != null)
          LButton(
            label: '查看服务端节点',
            variant: BtnVariant.solid,
            onPressed: () {
              ref.read(selectedNodeProvider.notifier).select(joinedNode.id);
              ref.read(sectionProvider.notifier).select(Section.nodes);
              Navigator.pop(context);
            },
          )
        else if (joined != null)
          LButton(
            label: '查看客户端',
            variant: BtnVariant.solid,
            onPressed: () {
              ref.read(selectedClientProvider.notifier).select(joined.id);
              ref.read(sectionProvider.notifier).select(Section.clients);
              Navigator.pop(context);
            },
          )
        else
          LButton(label: '完成', onPressed: () => Navigator.pop(context)),
      ],
    );
  }
}
