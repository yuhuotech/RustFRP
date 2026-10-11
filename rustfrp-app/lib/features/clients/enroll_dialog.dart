import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';

Future<void> showEnrollDialog(BuildContext context) =>
    showDialog<void>(context: context, builder: (_) => const EnrollDialog());

/// Wizard: name it → get the one-time install command → wait for the device to come online.
class EnrollDialog extends ConsumerStatefulWidget {
  const EnrollDialog({super.key});
  @override
  ConsumerState<EnrollDialog> createState() => _EnrollDialogState();
}

class _EnrollDialogState extends ConsumerState<EnrollDialog> {
  final _name = TextEditingController();
  int _ttl = 60;
  EnrollResult? _result;
  bool _busy = false;
  String _platform = 'linux_macos';
  Set<String> _knownClients = {};

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  Future<void> _create() async {
    setState(() => _busy = true);
    _knownClients = {...?ref.read(clientsProvider).asData?.value.map((c) => c.id)};
    final r = await guarded(context, () => ref.read(actionsProvider).createToken(_name.text.trim(), _ttl, '通过面板接入新客户端「${_name.text.trim()}」'));
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
    if (r == null) {
      return DialogShell(
        title: '接入新客户端',
        content: Column(crossAxisAlignment: CrossAxisAlignment.start, mainAxisSize: MainAxisSize.min, children: [
          Text('给这台机器起个名字，然后复制生成的命令到它的终端里执行。命令里的令牌只能使用一次。', style: Type.body.copyWith(color: c.text2)),
          const SizedBox(height: 14),
          LTextField(key: const Key('enroll-name'), controller: _name, hint: '例如：家里的 NAS', autofocus: true, onChanged: (_) => setState(() {})),
          const SizedBox(height: 12),
          Text('令牌有效期', style: Type.caption.copyWith(color: c.text3)),
          const SizedBox(height: 4),
          LSelect<int>(
            value: _ttl,
            options: const [LSelectOption(15, '15 分钟'), LSelectOption(60, '1 小时'), LSelectOption(1440, '24 小时'), LSelectOption(10080, '7 天')],
            onChanged: (v) => setState(() => _ttl = v ?? 60),
          ),
        ]),
        actions: [
          LButton(label: '取消', onPressed: () => Navigator.pop(context)),
          LButton(key: const Key('enroll-create'), label: '生成命令', variant: BtnVariant.solid, loading: _busy, onPressed: _name.text.trim().isEmpty ? null : _create),
        ],
      );
    }
    final cmds = r.commands;
    final platforms = [
      if (cmds.containsKey('linux_macos')) const LSelectOption('linux_macos', 'Linux / macOS（一行安装）'),
      if (cmds.containsKey('windows')) const LSelectOption('windows', 'Windows（PowerShell）'),
      const LSelectOption('direct', '已安装 agent，直接运行'),
    ];
    final clients = ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    final joined = clients.where((d) => !_knownClients.contains(d.id)).firstOrNull;
    return DialogShell(
      title: joined == null ? '在目标机器上执行' : '接入成功',
      maxWidth: 560,
      content: Column(crossAxisAlignment: CrossAxisAlignment.start, mainAxisSize: MainAxisSize.min, children: [
        if (joined == null) ...[
          LSelect<String>(value: _platform, options: platforms, onChanged: (v) => setState(() => _platform = v ?? 'direct')),
          const SizedBox(height: 12),
          CopyField(key: const Key('enroll-command'), text: cmds[_platform] ?? cmds['direct'] ?? ''),
          const SizedBox(height: 12),
          Text('令牌将在 ${remaining(r.expiresAt)} 后失效，用过一次即作废。', style: Type.caption.copyWith(color: c.text3)),
          const SizedBox(height: 14),
          Row(children: [
            const SizedBox(width: 14, height: 14, child: CircularProgressIndicator(strokeWidth: 2)),
            const SizedBox(width: 8),
            Text('等待设备上线…', style: Type.body.copyWith(color: c.text2)),
          ]),
        ] else
          Row(children: [
            DeviceGlyph(type: joined.os, size: 40, online: joined.online),
            const SizedBox(width: 12),
            Expanded(
              child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                Text(joined.name, key: const Key('enroll-joined'), style: Type.strong.copyWith(color: c.text1)),
                Text('${osLabel(joined.os)} · ${joined.arch} · agent ${joined.version}', style: Type.caption.copyWith(color: c.text3)),
              ]),
            ),
          ]),
      ]),
      actions: [
        if (joined != null)
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
