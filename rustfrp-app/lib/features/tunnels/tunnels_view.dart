import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';

Color stateColor(BuildContext context, TunnelState s) {
  final c = context.c;
  return switch (s) {
    TunnelState.running => c.success,
    TunnelState.error => c.danger,
    TunnelState.pending => c.warning,
    _ => c.controlOff,
  };
}

/// Wide layout list column: filter groups.
class TunnelFilterList extends ConsumerWidget {
  const TunnelFilterList({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final filter = ref.watch(tunnelFilterProvider);
    final tunnels = ref.watch(tunnelsProvider).asData?.value ?? const <Tunnel>[];
    int count(TunnelState? s) => s == null ? tunnels.length : tunnels.where((t) => t.state == s).length;
    final groups = <(TunnelState?, String)>[
      (null, '全部'),
      (TunnelState.running, '运行中'),
      (TunnelState.error, '异常'),
      (TunnelState.stopped, '已停止'),
      (TunnelState.pending, '待启动'),
    ];
    return ListView(padding: const EdgeInsets.all(8), children: [
      for (final g in groups)
        Padding(
          padding: const EdgeInsets.only(bottom: 2),
          child: HoverRow(
            key: Key('filter-${g.$1?.name ?? 'all'}'),
            selected: filter == g.$1,
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
            onTap: () => ref.read(tunnelFilterProvider.notifier).select(g.$1),
            child: Row(children: [
              Expanded(child: Text(g.$2, style: Type.body.copyWith(color: c.text1))),
              Text('${count(g.$1)}', style: Type.caption.copyWith(color: c.text3)),
            ]),
          ),
        ),
    ]);
  }
}

class TunnelsView extends ConsumerWidget {
  const TunnelsView({super.key, this.showFilter = false});

  /// Narrow screens show the filter as chips above the list (there is no list column).
  final bool showFilter;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final filter = ref.watch(tunnelFilterProvider);
    final tunnels = ref.watch(tunnelsProvider);
    final clients = ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    return Column(children: [
      PageHeader(title: '隧道', actions: [
        if (role.atLeast(Role.operator))
          LButton(
            key: const Key('new-tunnel'),
            label: '新建隧道',
            icon: LucideIcons.plus,
            variant: BtnVariant.solid,
            onPressed: clients.isEmpty ? null : () => showTunnelEditor(context),
          ),
      ]),
      if (showFilter)
        SingleChildScrollView(
          scrollDirection: Axis.horizontal,
          padding: const EdgeInsets.fromLTRB(16, 10, 16, 0),
          child: Row(children: [
            for (final g in [(null, '全部'), (TunnelState.running, '运行中'), (TunnelState.error, '异常'), (TunnelState.stopped, '已停止')])
              Padding(
                padding: const EdgeInsets.only(right: 6),
                child: LButton(
                  label: g.$2,
                  compact: true,
                  variant: filter == g.$1 ? BtnVariant.solid : BtnVariant.neutral,
                  onPressed: () => ref.read(tunnelFilterProvider.notifier).select(g.$1),
                ),
              ),
          ]),
        ),
      Expanded(
        child: AsyncView<List<Tunnel>>(
          value: tunnels,
          onRetry: () => ref.invalidate(tunnelsProvider),
          builder: (all) {
            final list = filter == null ? all : all.where((t) => t.state == filter).toList();
            if (list.isEmpty) {
              return EmptyState(
                icon: LucideIcons.waypoints,
                title: all.isEmpty ? '还没有隧道' : '没有符合条件的隧道',
                message: all.isEmpty ? (clients.isEmpty ? '先接入一台客户端，再为它创建隧道。' : '点击右上角「新建隧道」把内网服务暴露出来。') : null,
              );
            }
            return ListView(padding: const EdgeInsets.all(24), children: [
              for (final t in list) Padding(padding: const EdgeInsets.only(bottom: 8), child: TunnelCard(tunnel: t)),
              if (clients.isEmpty) Text('', style: Type.caption.copyWith(color: c.text3)),
            ]);
          },
        ),
      ),
    ]);
  }
}

/// What visitors type to reach a tunnel: `host:port` for tcp / udp, a URL for http / https.
String publicAddress(Tunnel t, NodeInfo? node) {
  final host = node?.publicHost ?? '';
  if (t.isWeb) {
    final port = t.kind == 'http' ? node?.vhostHttpPort : node?.vhostHttpsPort;
    final hosts = [
      ...t.customDomains,
      if (t.subdomain.isNotEmpty && (node?.subdomainHost ?? '').isNotEmpty) '${t.subdomain}.${node!.subdomainHost}',
    ];
    if (hosts.isEmpty) return '未配置域名';
    final standard = (t.kind == 'http' && port == 80) || (t.kind == 'https' && port == 443);
    final suffix = port == null || standard ? '' : ':$port';
    return '${t.kind}://${hosts.first}$suffix${hosts.length > 1 ? '  (+${hosts.length - 1})' : ''}';
  }
  if (t.publicPort == 0) return '自动分配';
  return host.isEmpty ? ':${t.publicPort}' : hostPort(host, t.publicPort);
}

class TunnelCard extends ConsumerWidget {
  const TunnelCard({super.key, required this.tunnel, this.showClient = true});
  final Tunnel tunnel;
  final bool showClient;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final node = ref.watch(nodeProvider).asData?.value;
    final clients = ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    final t = tunnel;
    final owner = clients.where((d) => d.id == t.clientId).firstOrNull;
    final remote = publicAddress(t, node);
    final canEdit = role.atLeast(Role.operator);
    return PanelCard(
      key: Key('tunnel-${t.id}'),
      child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Row(children: [
          StatusDot(color: stateColor(context, t.state)),
          const SizedBox(width: 8),
          // One Expanded group (not Flexible + Spacer: two flex children would split the free space and
          // leave a gap before the controls).
          Expanded(
            child: Row(children: [
              Flexible(child: Text(t.name, style: Type.strong.copyWith(color: c.text1), overflow: TextOverflow.ellipsis)),
              const SizedBox(width: 8),
              LBadge(t.kind.toUpperCase()),
              const SizedBox(width: 6),
              LBadge(t.enabled ? t.state.label : '已停用',
                  bg: t.state == TunnelState.error && t.enabled ? c.dangerSoft : null,
                  fg: t.state == TunnelState.error && t.enabled ? c.dangerText : null),
            ]),
          ),
          if (canEdit) ...[
            LSwitch(value: t.enabled, onChanged: (v) => guarded(context, () => ref.read(actionsProvider).updateTunnel(t.id, enabled: v, reason: v ? '通过面板启用隧道' : '通过面板停用隧道'))),
            const SizedBox(width: 4),
            LIconButton(icon: LucideIcons.pencil, tooltip: '编辑', onPressed: () => showTunnelEditor(context, existing: t)),
            LIconButton(icon: LucideIcons.trash2, tooltip: '删除', onPressed: () => _delete(context, ref)),
          ],
        ]),
        const SizedBox(height: 8),
        Wrap(spacing: 16, runSpacing: 4, children: [
          Text('${t.localIp}:${t.localPort}  →  $remote', style: Type.caption.copyWith(color: c.text2, fontFamily: 'monospace', fontFamilyFallback: Type.monoFallback)),
          if (showClient && owner != null) Text(owner.name, style: Type.caption.copyWith(color: c.text3)),
        ]),
        if (t.state == TunnelState.error && t.statusError.isNotEmpty) ...[
          const SizedBox(height: 6),
          Text(t.statusError, style: Type.caption.copyWith(color: c.dangerText)),
        ],
      ]),
    );
  }

  Future<void> _delete(BuildContext context, WidgetRef ref) async {
    final reason = await askReason(context, title: '删除隧道', message: '删除「${tunnel.name}」后，公网端口会立即关闭。', confirm: '删除', destructive: true);
    if (reason == null || !context.mounted) return;
    await guarded(context, () => ref.read(actionsProvider).deleteTunnel(tunnel.id, reason));
  }
}

Future<void> showTunnelEditor(BuildContext context, {Tunnel? existing, String? clientId}) =>
    showDialog<void>(context: context, builder: (_) => TunnelEditor(existing: existing, clientId: clientId));

/// Create / edit a TCP tunnel. Server-side validation (port conflicts, allowed range) is surfaced as a toast.
class TunnelEditor extends ConsumerStatefulWidget {
  const TunnelEditor({super.key, this.existing, this.clientId});
  final Tunnel? existing;
  final String? clientId;
  @override
  ConsumerState<TunnelEditor> createState() => _TunnelEditorState();
}

class _TunnelEditorState extends ConsumerState<TunnelEditor> {
  late final _name = TextEditingController(text: widget.existing?.name ?? '');
  late final _localIp = TextEditingController(text: widget.existing?.localIp ?? '127.0.0.1');
  late final _localPort = TextEditingController(text: widget.existing == null ? '' : '${widget.existing!.localPort}');
  late final _remotePort = TextEditingController(text: (widget.existing?.remotePort ?? 0) == 0 ? '' : '${widget.existing!.remotePort}');
  late String? _client = widget.existing?.clientId ?? widget.clientId;
  late String _kind = widget.existing?.kind ?? 'tcp';
  late final _domains = TextEditingController(text: widget.existing?.customDomains.join(', ') ?? '');
  late final _sub = TextEditingController(text: widget.existing?.subdomain ?? '');
  late final _rewrite = TextEditingController(text: widget.existing?.hostHeaderRewrite ?? '');
  bool _busy = false;

  @override
  void dispose() {
    for (final c in [_name, _localIp, _localPort, _remotePort, _domains, _sub, _rewrite]) {
      c.dispose();
    }
    super.dispose();
  }

  int? get _lp => int.tryParse(_localPort.text.trim());
  int? get _rp => _remotePort.text.trim().isEmpty ? 0 : int.tryParse(_remotePort.text.trim());
  bool get _web => _kind == 'http' || _kind == 'https';
  List<String> get _domainList => _domains.text.split(RegExp(r'[,\s]+')).where((d) => d.isNotEmpty).toList();
  bool get _valid =>
      _client != null &&
      _name.text.trim().isNotEmpty &&
      _localIp.text.trim().isNotEmpty &&
      (_lp ?? 0) >= 1 &&
      (_lp ?? 0) <= 65535 &&
      (_web ? (_domainList.isNotEmpty || _sub.text.trim().isNotEmpty) : (_rp != null && _rp! >= 0 && _rp! <= 65535));

  Future<void> _save() async {
    setState(() => _busy = true);
    final a = ref.read(actionsProvider);
    final e = widget.existing;
    final ok = await guarded(context, () async {
      if (e == null) {
        await a.createTunnel(
            clientId: _client!,
            name: _name.text.trim(),
            localPort: _lp!,
            localIp: _localIp.text.trim(),
            remotePort: _web ? 0 : _rp!,
            kind: _kind,
            customDomains: _web ? _domainList : const [],
            subdomain: _web ? _sub.text.trim() : '',
            hostHeaderRewrite: _kind == 'http' ? _rewrite.text.trim() : '',
            reason: '通过面板创建隧道');
      } else {
        await a.updateTunnel(
          e.id,
          name: _name.text.trim(),
          localIp: _localIp.text.trim(),
          localPort: _lp,
          remotePort: _web ? null : _rp,
          customDomains: _web ? _domainList : null,
          subdomain: _web ? _sub.text.trim() : null,
          hostHeaderRewrite: _kind == 'http' ? _rewrite.text.trim() : null,
          reason: '通过面板修改隧道');
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
    final clients = ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    final editing = widget.existing != null;
    Widget label(String t) => Padding(padding: const EdgeInsets.only(bottom: 4), child: Text(t, style: Type.caption.copyWith(color: c.text3)));
    return DialogShell(
      title: editing ? '编辑隧道' : '新建隧道',
      content: Column(crossAxisAlignment: CrossAxisAlignment.start, mainAxisSize: MainAxisSize.min, children: [
        label('客户端'),
        if (clients.isEmpty || editing)
          Text(clients.where((d) => d.id == _client).firstOrNull?.name ?? '—', style: Type.body.copyWith(color: c.text1))
        else
          LSelect<String?>(
            value: _client,
            options: [
              if (_client == null) const LSelectOption<String?>(null, '选择客户端'),
              for (final d in clients) LSelectOption<String?>(d.id, d.name),
            ],
            onChanged: (v) => setState(() => _client = v),
          ),
        const SizedBox(height: 12),
        label('类型'),
        if (editing)
          Text(_kind.toUpperCase(), style: Type.body.copyWith(color: c.text1))
        else
          LSelect<String>(
            value: _kind,
            options: const [
              LSelectOption('tcp', 'TCP'),
              LSelectOption('udp', 'UDP'),
              LSelectOption('http', 'HTTP（按域名）'),
              LSelectOption('https', 'HTTPS（按域名，TLS 透传）'),
            ],
            onChanged: (v) => setState(() => _kind = v ?? 'tcp'),
          ),
        const SizedBox(height: 12),
        label('名称'),
        LTextField(key: const Key('tunnel-name'), controller: _name, hint: '例如：ssh', onChanged: (_) => setState(() {})),
        const SizedBox(height: 12),
        Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Expanded(flex: 3, child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            label('本地地址'),
            LTextField(key: const Key('tunnel-local-ip'), controller: _localIp, onChanged: (_) => setState(() {})),
          ])),
          const SizedBox(width: 8),
          Expanded(flex: 2, child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            label('本地端口'),
            LTextField(key: const Key('tunnel-local-port'), controller: _localPort, hint: '22', onChanged: (_) => setState(() {})),
          ])),
        ]),
        const SizedBox(height: 12),
        if (!_web) ...[
          label('公网端口（留空 = 自动分配）'),
          LTextField(key: const Key('tunnel-remote-port'), controller: _remotePort, hint: '6000', onChanged: (_) => setState(() {})),
        ] else ...[
          label('自定义域名（多个用逗号分隔，支持 *.example.com）'),
          LTextField(key: const Key('tunnel-domains'), controller: _domains, hint: 'www.example.com', onChanged: (_) => setState(() {})),
          const SizedBox(height: 12),
          label('子域名（需要服务器配置子域名后缀）'),
          LTextField(key: const Key('tunnel-subdomain'), controller: _sub, hint: 'blog', onChanged: (_) => setState(() {})),
          if (_kind == 'http') ...[
            const SizedBox(height: 12),
            label('改写 Host 头（可选）'),
            LTextField(key: const Key('tunnel-rewrite'), controller: _rewrite, hint: 'internal.lan:8080'),
          ],
        ],
      ]),
      actions: [
        LButton(label: '取消', onPressed: () => Navigator.pop(context)),
        LButton(key: const Key('tunnel-save'), label: '保存', variant: BtnVariant.solid, loading: _busy, onPressed: _valid ? _save : null),
      ],
    );
  }
}
