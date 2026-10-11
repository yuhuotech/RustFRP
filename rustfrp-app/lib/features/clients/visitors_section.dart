import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';

/// 客户端详情里的「访问者」区块：在这台设备上监听本机端口，把连接送到其他设备的 stcp / sudp 隧道。
class VisitorsSection extends ConsumerWidget {
  const VisitorsSection({super.key, required this.clientId});
  final String clientId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final visitors = [
      for (final v
          in ref.watch(visitorsProvider).asData?.value ?? const <Visitor>[])
        if (v.clientId == clientId) v,
    ];
    final tunnels =
        ref.watch(tunnelsProvider).asData?.value ?? const <Tunnel>[];
    final clients =
        ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    String target(Visitor v) {
      final t = tunnels.where((x) => x.id == v.tunnelId).firstOrNull;
      if (t == null) return '目标隧道已删除';
      final owner = clients.where((d) => d.id == t.clientId).firstOrNull;
      return '${owner?.name ?? '?'} / ${t.name}（${t.kind.toUpperCase()}）';
    }

    final canEdit = role.atLeast(Role.operator);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SectionTitle(
          '访问者（${visitors.length}）',
          trailing: canEdit
              ? LButton(
                  key: const Key('client-new-visitor'),
                  label: '新建访问者',
                  icon: LucideIcons.plus,
                  compact: true,
                  onPressed: () => showVisitorEditor(context, clientId),
                )
              : null,
        ),
        if (visitors.isEmpty)
          PanelCard(
            child: Text(
              '访问者让这台设备通过本机端口访问其他设备上的私密（STCP / SUDP）隧道。',
              style: Type.body.copyWith(color: c.text3),
            ),
          )
        else
          for (final v in visitors)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: PanelCard(
                key: Key('visitor-${v.id}'),
                child: Row(
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            v.name,
                            style: Type.strong.copyWith(color: c.text1),
                          ),
                          const SizedBox(height: 4),
                          Text(
                            '${v.bindAddr}:${v.bindPort == 0 ? '自动' : v.bindPort}  →  ${target(v)}',
                            style: Type.caption.copyWith(
                              color: c.text2,
                              fontFamily: 'monospace',
                              fontFamilyFallback: Type.monoFallback,
                            ),
                          ),
                        ],
                      ),
                    ),
                    if (canEdit) ...[
                      LSwitch(
                        value: v.enabled,
                        onChanged: (on) => guarded(
                          context,
                          () => ref
                              .read(actionsProvider)
                              .updateVisitor(
                                v.id,
                                enabled: on,
                                reason: on ? '通过面板启用访问者' : '通过面板停用访问者',
                              ),
                        ),
                      ),
                      const SizedBox(width: 4),
                      LIconButton(
                        icon: LucideIcons.pencil,
                        tooltip: '编辑访问者',
                        onPressed: () =>
                            showVisitorEditor(context, clientId, existing: v),
                      ),
                      LIconButton(
                        icon: LucideIcons.trash2,
                        tooltip: '删除访问者',
                        onPressed: () => _delete(context, ref, v),
                      ),
                    ],
                  ],
                ),
              ),
            ),
      ],
    );
  }

  Future<void> _delete(BuildContext context, WidgetRef ref, Visitor v) async {
    final reason = await askReason(
      context,
      title: '删除访问者',
      message: '删除「${v.name}」后，这台设备上的本机端口会立即关闭。',
      confirm: '删除',
      destructive: true,
    );
    if (reason == null || !context.mounted) return;
    await guarded(
      context,
      () => ref.read(actionsProvider).deleteVisitor(v.id, reason),
    );
  }
}

Future<void> showVisitorEditor(
  BuildContext context,
  String clientId, {
  Visitor? existing,
}) => showDialog<void>(
  context: context,
  builder: (_) => VisitorEditor(clientId: clientId, existing: existing),
);

class VisitorEditor extends ConsumerStatefulWidget {
  const VisitorEditor({super.key, required this.clientId, this.existing});
  final String clientId;
  final Visitor? existing;

  @override
  ConsumerState<VisitorEditor> createState() => _VisitorEditorState();
}

class _VisitorEditorState extends ConsumerState<VisitorEditor> {
  late final _name = TextEditingController(text: widget.existing?.name ?? '');
  late final _addr = TextEditingController(
    text: widget.existing?.bindAddr ?? '127.0.0.1',
  );
  late final _port = TextEditingController(
    text: (widget.existing?.bindPort ?? 0) == 0
        ? ''
        : '${widget.existing!.bindPort}',
  );
  late String? _tunnel = widget.existing?.tunnelId;
  bool _busy = false;

  @override
  void dispose() {
    for (final c in [_name, _addr, _port]) {
      c.dispose();
    }
    super.dispose();
  }

  int? get _portValue {
    final t = _port.text.trim();
    if (t.isEmpty) return 0;
    final n = int.tryParse(t);
    return n == null || n < 0 || n > 65535 ? null : n;
  }

  bool get _valid =>
      _name.text.trim().isNotEmpty &&
      _addr.text.trim().isNotEmpty &&
      _tunnel != null &&
      _portValue != null;

  Future<void> _save() async {
    setState(() => _busy = true);
    final a = ref.read(actionsProvider);
    final e = widget.existing;
    final ok = await guarded(context, () async {
      if (e == null) {
        await a.createVisitor(
          clientId: widget.clientId,
          name: _name.text.trim(),
          tunnelId: _tunnel!,
          bindAddr: _addr.text.trim(),
          bindPort: _portValue!,
          reason: '通过面板创建访问者',
        );
      } else {
        await a.updateVisitor(
          e.id,
          name: _name.text.trim(),
          tunnelId: _tunnel,
          bindAddr: _addr.text.trim(),
          bindPort: _portValue,
          reason: '通过面板修改访问者',
        );
      }
      return true;
    });
    if (!mounted) return;
    setState(() => _busy = false);
    if (ok == true) Navigator.pop(context);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final tunnels = [
      for (final t
          in ref.watch(tunnelsProvider).asData?.value ?? const <Tunnel>[])
        if (t.isSecret && t.clientId != widget.clientId) t,
    ];
    final clients =
        ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    String owner(Tunnel t) =>
        clients.where((d) => d.id == t.clientId).firstOrNull?.name ?? '?';
    Widget label(String t) => Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Text(t, style: Type.caption.copyWith(color: c.text3)),
    );
    return DialogShell(
      title: widget.existing == null ? '新建访问者' : '编辑访问者',
      content: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          label('名称'),
          LTextField(
            key: const Key('visitor-name'),
            controller: _name,
            hint: '例如：nas-ssh',
            onChanged: (_) => setState(() {}),
          ),
          const SizedBox(height: 12),
          label('目标隧道（其他客户端上的 STCP / SUDP 隧道）'),
          if (tunnels.isEmpty && _tunnel == null)
            Text(
              '还没有可访问的私密隧道：先在其他客户端上创建 STCP / SUDP 隧道。',
              style: Type.caption.copyWith(color: c.warningText),
            )
          else
            LSelect<String?>(
              key: const Key('visitor-tunnel'),
              value: _tunnel,
              options: [
                if (_tunnel == null) const LSelectOption<String?>(null, '选择隧道'),
                for (final t in tunnels)
                  LSelectOption<String?>(
                    t.id,
                    '${owner(t)} / ${t.name}（${t.kind.toUpperCase()}）',
                  ),
                if (_tunnel != null && !tunnels.any((t) => t.id == _tunnel))
                  LSelectOption<String?>(_tunnel, '当前目标（不可用）'),
              ],
              onChanged: (v) => setState(() => _tunnel = v),
            ),
          const SizedBox(height: 12),
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                flex: 3,
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    label('本机监听地址'),
                    LTextField(
                      key: const Key('visitor-addr'),
                      controller: _addr,
                      onChanged: (_) => setState(() {}),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 8),
              Expanded(
                flex: 2,
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    label('端口（留空 = 自动）'),
                    LTextField(
                      key: const Key('visitor-port'),
                      controller: _port,
                      hint: '2222',
                      onChanged: (_) => setState(() {}),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ],
      ),
      actions: [
        LButton(label: '取消', onPressed: () => Navigator.pop(context)),
        LButton(
          key: const Key('visitor-save'),
          label: '保存',
          variant: BtnVariant.solid,
          loading: _busy,
          onPressed: _valid && !_busy ? _save : null,
        ),
      ],
    );
  }
}
