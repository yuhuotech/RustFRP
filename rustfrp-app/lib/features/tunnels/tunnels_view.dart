import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../shared/workspace_widgets.dart';
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

class TunnelsView extends ConsumerStatefulWidget {
  const TunnelsView({super.key, this.showFilter = false});

  /// Both wide and narrow workspaces show filters above the tunnel list.
  final bool showFilter;

  @override
  ConsumerState<TunnelsView> createState() => _TunnelsViewState();
}

class _TunnelsViewState extends ConsumerState<TunnelsView> {
  final Set<String> selected = {};
  bool busy = false;
  Future<void> batch(bool enabled) async {
    if (busy) {
      return;
    }
    setState(() => busy = true);
    final success = await guarded(context, () async {
      await ref
          .read(actionsProvider)
          .batchTunnels(
            selected.toList(),
            enabled,
            enabled ? '通过面板批量启用隧道' : '通过面板批量停用隧道',
          );
      return true;
    });
    if (mounted) {
      setState(() {
        busy = false;
        if (success == true) {
          selected.clear();
        }
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final filter = ref.watch(tunnelFilterProvider);
    final tunnels = ref.watch(tunnelsProvider);
    final clients =
        ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    if (tunnels.hasValue) {
      selected.retainAll(tunnels.requireValue.map((t) => t.id));
    }
    return Column(
      children: [
        PageHeader(
          title: '隧道管理',
          actions: [
            if (role.atLeast(Role.operator))
              LButton(
                key: const Key('new-tunnel'),
                label: '新建隧道',
                icon: LucideIcons.plus,
                variant: BtnVariant.solid,
                onPressed: clients.isEmpty
                    ? null
                    : () => showTunnelEditor(context),
              ),
          ],
        ),
        if (widget.showFilter)
          Align(
            alignment: Alignment.centerLeft,
            child: SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              padding: EdgeInsets.fromLTRB(
                isNarrow(context) ? 16 : 24,
                10,
                isNarrow(context) ? 16 : 24,
                0,
              ),
              child: Row(
                children: [
                  for (final g in [
                    (null, '全部'),
                    (TunnelState.running, '运行中'),
                    (TunnelState.error, '异常'),
                    (TunnelState.stopped, '已停止'),
                    (TunnelState.pending, '待启动'),
                  ])
                    Padding(
                      padding: const EdgeInsets.only(right: 6),
                      child: LButton(
                        key: Key('filter-${g.$1?.name ?? 'all'}'),
                        label: g.$2,
                        compact: true,
                        variant: filter == g.$1
                            ? BtnVariant.solid
                            : BtnVariant.neutral,
                        onPressed: () => ref
                            .read(tunnelFilterProvider.notifier)
                            .select(g.$1),
                      ),
                    ),
                ],
              ),
            ),
          ),
        if (role.atLeast(Role.operator) && selected.isNotEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 8),
            child: Wrap(
              spacing: 8,
              runSpacing: 8,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                Text(
                  '已选 ${selected.length} 条',
                  style: Type.caption.copyWith(color: c.text2),
                ),
                LButton(
                  key: const Key('batch-enable'),
                  label: '批量启用',
                  loading: busy,
                  onPressed: busy ? null : () => batch(true),
                ),
                LButton(
                  key: const Key('batch-disable'),
                  label: '批量停用',
                  loading: busy,
                  onPressed: busy ? null : () => batch(false),
                ),
                LButton(
                  label: '清除选择',
                  onPressed: busy ? null : () => setState(selected.clear),
                ),
              ],
            ),
          ),
        Expanded(
          child: AsyncView<List<Tunnel>>(
            value: tunnels,
            onRetry: () => ref.invalidate(tunnelsProvider),
            builder: (all) {
              final list = filter == null
                  ? all
                  : all.where((t) => t.state == filter).toList();
              if (list.isEmpty) {
                return EmptyState(
                  icon: LucideIcons.waypoints,
                  title: all.isEmpty ? '还没有隧道' : '没有符合条件的隧道',
                  message: all.isEmpty
                      ? (clients.isEmpty
                            ? '先接入一台客户端，再为它创建隧道。'
                            : '点击右上角「新建隧道」把内网服务暴露出来。')
                      : null,
                );
              }
              return ListView(
                padding: const EdgeInsets.all(24),
                children: [
                  PageIntro(
                    icon: LucideIcons.waypoints,
                    title: '隧道与公网服务',
                    description: '比较公网入口和本地目标，按状态筛选并管理转发规则。选择多条隧道可批量启用或停用。',
                    color: c.actionText,
                    badges: [
                      LBadge('显示 ${list.length} / ${all.length} 条'),
                      LBadge(
                        '${all.where((t) => t.state == TunnelState.running).length} 条运行中',
                        bg: c.successSoft,
                        fg: c.successText,
                      ),
                    ],
                  ),
                  if (!isNarrow(context))
                    ResourceTable(
                      columns: const [
                        ('隧道 / 类型', 3),
                        ('状态', 2),
                        ('客户端', 2),
                        ('公网入口 / 本地目标', 5),
                        ('操作', 3),
                      ],
                      rows: [
                        for (final t in list)
                          TunnelCard(
                            tunnel: t,
                            tableRow: true,
                            selection: role.atLeast(Role.operator)
                                ? LButton(
                                    key: Key('select-tunnel-${t.id}'),
                                    label: selected.contains(t.id)
                                        ? '已选择'
                                        : '选择',
                                    compact: true,
                                    onPressed: busy
                                        ? null
                                        : () => setState(() {
                                            if (selected.contains(t.id)) {
                                              selected.remove(t.id);
                                            } else if (selected.length < 100) {
                                              selected.add(t.id);
                                            }
                                          }),
                                  )
                                : null,
                          ),
                      ],
                    )
                  else
                    for (final t in list)
                      Padding(
                        padding: const EdgeInsets.only(bottom: 8),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            if (role.atLeast(Role.operator))
                              Padding(
                                padding: const EdgeInsets.only(bottom: 4),
                                child: LButton(
                                  key: Key('select-tunnel-${t.id}'),
                                  label: selected.contains(t.id) ? '已选择' : '选择',
                                  compact: true,
                                  onPressed: busy
                                      ? null
                                      : () => setState(() {
                                          if (selected.contains(t.id)) {
                                            selected.remove(t.id);
                                          } else if (selected.length < 100) {
                                            selected.add(t.id);
                                          }
                                        }),
                                ),
                              ),
                            TunnelCard(tunnel: t),
                          ],
                        ),
                      ),
                  if (clients.isEmpty)
                    Text('', style: Type.caption.copyWith(color: c.text3)),
                ],
              );
            },
          ),
        ),
      ],
    );
  }
}

/// What visitors type to reach a tunnel: `host:port` for tcp / udp, a URL for http / https.
String publicAddress(Tunnel t, NodeInfo? node) {
  final host = node?.publicHost ?? '';
  if (t.kind == 'tcpmux') {
    final domain =
        t.customDomains.firstOrNull ??
        (t.subdomain.isNotEmpty && (node?.subdomainHost ?? '').isNotEmpty
            ? '${t.subdomain}.${node!.subdomainHost}'
            : null);
    final port = node?.tcpmuxHttpConnectPort;
    if (domain == null) return '未配置 TCPMUX 域名';
    return port == null ? 'CONNECT $domain' : 'CONNECT $domain:$port';
  }
  if (t.isWeb) {
    final port = t.kind == 'http' ? node?.vhostHttpPort : node?.vhostHttpsPort;
    final hosts = [
      ...t.customDomains,
      if (t.subdomain.isNotEmpty && (node?.subdomainHost ?? '').isNotEmpty)
        '${t.subdomain}.${node!.subdomainHost}',
    ];
    if (hosts.isEmpty) return '未配置域名';
    final standard =
        (t.kind == 'http' && port == 80) || (t.kind == 'https' && port == 443);
    final suffix = port == null || standard ? '' : ':$port';
    return '${t.kind}://${hosts.first}$suffix${hosts.length > 1 ? '  (+${hosts.length - 1})' : ''}';
  }
  if (t.isSecret) return '私密（经访问者）';
  if (t.publicPort == 0) return '自动分配';
  return host.isEmpty ? ':${t.publicPort}' : hostPort(host, t.publicPort);
}

class TunnelCard extends ConsumerWidget {
  const TunnelCard({
    super.key,
    required this.tunnel,
    this.showClient = true,
    this.tableRow = false,
    this.selection,
  });
  final Tunnel tunnel;
  final bool showClient, tableRow;
  final Widget? selection;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final clients =
        ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    final t = tunnel;
    final owner = clients.where((d) => d.id == t.clientId).firstOrNull;
    // 隧道跑在设备所属的节点上：内置节点用 /node，外置节点用 /nodes 里的那一项。
    final builtin = ref.watch(nodeProvider).asData?.value;
    final node = (owner?.nodeId ?? '').isEmpty
        ? builtin
        : ref
              .watch(nodesProvider)
              .asData
              ?.value
              .where((n) => n.id == owner!.nodeId)
              .firstOrNull;
    final remote = publicAddress(t, node);
    final canEdit = role.atLeast(Role.operator);
    final controls = Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (canEdit) ...[
          LSwitch(
            value: t.enabled,
            onChanged: (v) => guarded(
              context,
              () => ref
                  .read(actionsProvider)
                  .updateTunnel(
                    t.id,
                    enabled: v,
                    reason: v ? '通过面板启用隧道' : '通过面板停用隧道',
                  ),
            ),
          ),
          const SizedBox(width: 4),
          LIconButton(
            icon: LucideIcons.pencil,
            tooltip: '编辑',
            onPressed: () => showTunnelEditor(context, existing: t),
          ),
          LIconButton(
            icon: LucideIcons.trash2,
            tooltip: '删除',
            onPressed: () => _delete(context, ref),
          ),
        ],
      ],
    );
    final status = LBadge(
      t.enabled ? t.state.label : '已停用',
      bg: !t.enabled
          ? c.bgSubtle
          : switch (t.state) {
              TunnelState.running => c.successSoft,
              TunnelState.error => c.dangerSoft,
              TunnelState.pending => c.warningSoft,
              _ => c.bgSubtle,
            },
      fg: !t.enabled
          ? c.text2
          : switch (t.state) {
              TunnelState.running => c.successText,
              TunnelState.error => c.dangerText,
              TunnelState.pending => c.warningText,
              _ => c.text2,
            },
    );
    if (tableRow) {
      return ResourceRow(
        key: Key('tunnel-${t.id}'),
        cells: [
          (
            Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                TableText(t.name),
                const SizedBox(height: 6),
                Wrap(
                  spacing: 8,
                  runSpacing: 4,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    LBadge(
                      t.kind.toUpperCase(),
                      bg: c.directSoft,
                      fg: c.directText,
                    ),
                    ?selection,
                  ],
                ),
              ],
            ),
            3,
          ),
          (
            Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                status,
                if (t.state == TunnelState.error &&
                    t.statusError.isNotEmpty) ...[
                  const SizedBox(height: 4),
                  TableText(t.statusError, color: c.dangerText),
                ],
              ],
            ),
            2,
          ),
          (TableText(owner?.name ?? '未知客户端', subtitle: node?.name), 2),
          (
            TableText(
              remote,
              subtitle: '本地目标 ${t.localIp}:${t.localPort}',
              color: c.directText,
              mono: true,
            ),
            5,
          ),
          (
            canEdit
                ? controls
                : Text('只读', style: Type.caption.copyWith(color: c.text3)),
            3,
          ),
        ],
      );
    }
    return PanelCard(
      key: Key('tunnel-${t.id}'),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              StatusDot(color: stateColor(context, t.state)),
              const SizedBox(width: 8),
              // One Expanded group (not Flexible + Spacer: two flex children would split the free space and
              // leave a gap before the controls).
              Expanded(
                child: Row(
                  children: [
                    Flexible(
                      child: Text(
                        t.name,
                        style: Type.strong.copyWith(color: c.text1),
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                    const SizedBox(width: 8),
                    LBadge(
                      t.kind.toUpperCase(),
                      bg: c.directSoft,
                      fg: c.directText,
                    ),
                    const SizedBox(width: 6),
                    status,
                  ],
                ),
              ),
              if (canEdit) controls,
            ],
          ),
          const SizedBox(height: 8),
          Wrap(
            spacing: 16,
            runSpacing: 4,
            children: [
              Text(
                '${t.localIp}:${t.localPort}  →  $remote',
                style: Type.caption.copyWith(
                  color: c.text2,
                  fontFamily: 'monospace',
                  fontFamilyFallback: Type.monoFallback,
                ),
              ),
              if (showClient && owner != null)
                Text(owner.name, style: Type.caption.copyWith(color: c.text3)),
            ],
          ),
          if (t.state == TunnelState.error && t.statusError.isNotEmpty) ...[
            const SizedBox(height: 6),
            Text(
              t.statusError,
              style: Type.caption.copyWith(color: c.dangerText),
            ),
          ],
        ],
      ),
    );
  }

  Future<void> _delete(BuildContext context, WidgetRef ref) async {
    final reason = await askReason(
      context,
      title: '删除隧道',
      message: '删除「${tunnel.name}」后，公网端口会立即关闭。',
      confirm: '删除',
      destructive: true,
    );
    if (reason == null || !context.mounted) return;
    await guarded(
      context,
      () => ref.read(actionsProvider).deleteTunnel(tunnel.id, reason),
    );
  }
}

Future<void> showTunnelEditor(
  BuildContext context, {
  Tunnel? existing,
  String? clientId,
}) => showDialog<void>(
  context: context,
  builder: (_) => TunnelEditor(existing: existing, clientId: clientId),
);

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
  late final _localIp = TextEditingController(
    text: widget.existing?.localIp ?? '127.0.0.1',
  );
  late final _localPort = TextEditingController(
    text: widget.existing == null ? '' : '${widget.existing!.localPort}',
  );
  late final _remotePort = TextEditingController(
    text: (widget.existing?.remotePort ?? 0) == 0
        ? ''
        : '${widget.existing!.remotePort}',
  );
  late String? _client = widget.existing?.clientId ?? widget.clientId;
  late String _kind = widget.existing?.kind ?? 'tcp';
  late final _domains = TextEditingController(
    text: widget.existing?.customDomains.join(', ') ?? '',
  );
  late final _sub = TextEditingController(
    text: widget.existing?.subdomain ?? '',
  );
  late final _rewrite = TextEditingController(
    text: widget.existing?.hostHeaderRewrite ?? '',
  );
  late final _locations = TextEditingController(
    text: widget.existing?.locations.join(', ') ?? '',
  );
  late final _httpUser = TextEditingController(
    text: widget.existing?.httpUser ?? '',
  );
  final _httpPassword = TextEditingController();
  late final _routeUser = TextEditingController(
    text: widget.existing?.routeByHttpUser ?? '',
  );
  late final _headers = TextEditingController(
    text:
        widget.existing?.requestHeaders.entries
            .map((e) => '${e.key}: ${e.value}')
            .join('\n') ??
        '',
  );
  late final _respHeaders = TextEditingController(
    text:
        widget.existing?.responseHeaders.entries
            .map((e) => '${e.key}: ${e.value}')
            .join('\n') ??
        '',
  );
  final _pluginJson = TextEditingController();
  bool _clearPassword = false;
  late int _proxyProtocol = widget.existing?.proxyProtocol ?? 0;
  late final _bandwidth = TextEditingController(
    text: (widget.existing?.bandwidthBps ?? 0) == 0
        ? ''
        : '${(widget.existing!.bandwidthBps / 1024).ceil()}',
  );
  late bool _bandwidthServer = widget.existing?.bandwidthServer ?? false;
  late final _maxConnections = TextEditingController(
    text: widget.existing == null || widget.existing!.maxConnections == 0
        ? ''
        : '${widget.existing!.maxConnections}',
  );
  late String _multiplexer = widget.existing?.multiplexer ?? 'httpconnect';
  late bool _useEncryption = widget.existing?.useEncryption ?? false;
  late bool _useCompression = widget.existing?.useCompression ?? false;
  late final _lbGroup = TextEditingController(
    text: widget.existing?.lbGroup ?? '',
  );
  final _lbKey = TextEditingController();
  late String _hcKind = widget.existing?.healthCheck?.kind ?? '';
  late final _hcPath = TextEditingController(
    text: widget.existing?.healthCheck?.path ?? '',
  );
  late final _hcTimeout = TextEditingController(
    text: '${widget.existing?.healthCheck?.timeoutSecs ?? 3}',
  );
  late final _hcMax = TextEditingController(
    text: '${widget.existing?.healthCheck?.maxFailed ?? 1}',
  );
  late final _hcInterval = TextEditingController(
    text: '${widget.existing?.healthCheck?.intervalSecs ?? 10}',
  );

  /// 当前表单对应的健康检查；不检查返回 null，数值非法返回 [_badHealth]。
  HealthCheck? get _health {
    if (_hcKind.isEmpty) return null;
    final t = int.tryParse(_hcTimeout.text.trim());
    final m = int.tryParse(_hcMax.text.trim());
    final i = int.tryParse(_hcInterval.text.trim());
    final path = _hcPath.text.trim();
    final ok =
        t != null &&
        t >= 1 &&
        t <= 60 &&
        m != null &&
        m >= 1 &&
        m <= 20 &&
        i != null &&
        i >= 1 &&
        i <= 3600 &&
        (_hcKind != 'http' || path.startsWith('/'));
    if (!ok) return _badHealth;
    return HealthCheck(
      kind: _hcKind,
      timeoutSecs: t,
      maxFailed: m,
      intervalSecs: i,
      path: _hcKind == 'http' ? path : '',
      headers: _hcKind == 'http'
          ? widget.existing?.healthCheck?.headers ?? const {}
          : const {},
    );
  }

  static const _badHealth = HealthCheck(kind: '!');

  /// 限速输入（KB/s）→ 字节/秒；留空 = 0（不限），非法 = null。
  int? get _bandwidthBps {
    final t = _bandwidth.text.trim();
    if (t.isEmpty) return 0;
    final n = int.tryParse(t);
    return n == null || n < 0 || n > 1 << 30 ? null : n * 1024;
  }

  int? get _maxConnectionCount {
    final text = _maxConnections.text.trim();
    if (text.isEmpty) return 0;
    final count = int.tryParse(text);
    return count == null || count < 0 || count > 1000000 ? null : count;
  }

  bool _busy = false;
  String _template = 'custom';
  void _applyTemplate(String? value) {
    if (value == null || value == 'custom') {
      setState(() => _template = 'custom');
      return;
    }
    const templates = {
      'ssh': ('SSH', 22, 'tcp'),
      'rdp': ('RDP', 3389, 'tcp'),
      'web': ('Web', 80, 'http'),
      'mysql': ('MySQL', 3306, 'tcp'),
      'postgres': ('PostgreSQL', 5432, 'tcp'),
    };
    final template = templates[value]!;
    setState(() {
      _template = value;
      _name.text = template.$1;
      _localPort.text = '${template.$2}';
      _localIp.text = '127.0.0.1';
      _kind = template.$3;
      _remotePort.clear();
    });
  }

  @override
  void dispose() {
    for (final c in [
      _name,
      _localIp,
      _localPort,
      _remotePort,
      _domains,
      _sub,
      _rewrite,
      _locations,
      _httpUser,
      _httpPassword,
      _routeUser,
      _headers,
      _respHeaders,
      _pluginJson,
      _bandwidth,
      _maxConnections,
      _lbGroup,
      _lbKey,
      _hcPath,
      _hcTimeout,
      _hcMax,
      _hcInterval,
    ]) {
      c.dispose();
    }
    super.dispose();
  }

  int? get _lp => int.tryParse(_localPort.text.trim());
  int? get _rp => _remotePort.text.trim().isEmpty
      ? 0
      : int.tryParse(_remotePort.text.trim());
  bool get _web => _kind == 'http' || _kind == 'https';
  bool get _domain => _web || _kind == 'tcpmux';

  /// stcp / sudp：没有公网端口，由其他客户端上的访问者连接。
  bool get _secret => _kind == 'stcp' || _kind == 'sudp';

  /// 数据报类型（udp / sudp）：没有限速、PROXY protocol。
  bool get _datagram => _kind == 'udp' || _kind == 'sudp';

  /// 能做负载均衡组和健康检查的类型。
  bool get _balanced => _kind == 'tcp' || _kind == 'http' || _kind == 'https';
  List<String> get _domainList => _domains.text
      .split(RegExp(r'[,\s]+'))
      .where((d) => d.isNotEmpty)
      .toList();
  List<String> get _locationList => _locations.text
      .split(RegExp(r'[,\s]+'))
      .where((l) => l.isNotEmpty)
      .toList();

  /// 每行 `Name: value`；有非空行缺少冒号时返回 null（表单不合法）。
  Map<String, String>? get _headerMap => _parseHeaders(_headers.text);
  Map<String, String>? get _respHeaderMap => _parseHeaders(_respHeaders.text);

  static Map<String, String>? _parseHeaders(String text) {
    final out = <String, String>{};
    for (final line in text.split('\n')) {
      if (line.trim().isEmpty) continue;
      final i = line.indexOf(':');
      if (i <= 0) return null;
      out[line.substring(0, i).trim()] = line.substring(i + 1).trim();
    }
    return out;
  }

  bool get _httpValid =>
      _kind != 'http' ||
      (_locationList.every((l) => l.startsWith('/')) &&
          _headerMap != null &&
          _respHeaderMap != null);
  Map<String, dynamic>? get _pluginOptions {
    final raw = _pluginJson.text.trim();
    if (raw.isEmpty) return null;
    try {
      final decoded = jsonDecode(raw);
      return decoded is Map<String, dynamic> ? decoded : null;
    } on FormatException {
      return null;
    }
  }

  bool get _pluginValid =>
      _pluginJson.text.trim().isEmpty ||
      (((_kind == 'tcp' || _kind == 'http') && _pluginOptions != null));
  bool get _valid =>
      _client != null &&
      _httpValid &&
      _pluginValid &&
      (_datagram ||
          (_bandwidthBps != null && (!_balanced || _health != _badHealth))) &&
      _maxConnectionCount != null &&
      (_kind != 'tcpmux' || _multiplexer == 'httpconnect') &&
      _name.text.trim().isNotEmpty &&
      _localIp.text.trim().isNotEmpty &&
      (_lp ?? 0) >= 1 &&
      (_lp ?? 0) <= 65535 &&
      (_domain
          ? (_domainList.isNotEmpty || _sub.text.trim().isNotEmpty)
          : _secret || (_rp != null && _rp! >= 0 && _rp! <= 65535));

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
          remotePort: _domain || _secret ? 0 : _rp!,
          kind: _kind,
          customDomains: _domain ? _domainList : const [],
          subdomain: _domain ? _sub.text.trim() : '',
          hostHeaderRewrite: _kind == 'http' ? _rewrite.text.trim() : '',
          locations: _kind == 'http' ? _locationList : const [],
          httpUser: _kind == 'http' ? _httpUser.text.trim() : '',
          httpPassword: _kind == 'http' ? _httpPassword.text : '',
          routeByHttpUser: _kind == 'http' ? _routeUser.text.trim() : '',
          requestHeaders: _kind == 'http' ? _headerMap! : const {},
          responseHeaders: _kind == 'http' ? _respHeaderMap! : const {},
          proxyProtocol: _balanced ? _proxyProtocol : 0,
          bandwidthBps: _datagram ? 0 : _bandwidthBps!,
          bandwidthServer: !_datagram && _bandwidthServer,
          maxConnections: _maxConnectionCount!,
          multiplexer: _kind == 'tcpmux' ? _multiplexer : '',
          useEncryption: _useEncryption,
          useCompression: _useCompression,
          lbGroup: _balanced ? _lbGroup.text.trim() : '',
          lbGroupKey: _balanced ? _lbKey.text : '',
          healthCheck: _balanced ? _health : null,
          plugin: _pluginOptions,
          reason: '通过面板创建隧道',
        );
      } else {
        await a.updateTunnel(
          e.id,
          name: _name.text.trim(),
          localIp: _localIp.text.trim(),
          localPort: _lp,
          remotePort: _domain || _secret ? null : _rp,
          customDomains: _domain ? _domainList : null,
          subdomain: _domain ? _sub.text.trim() : null,
          hostHeaderRewrite: _kind == 'http' ? _rewrite.text.trim() : null,
          locations: _kind == 'http' ? _locationList : null,
          httpUser: _kind == 'http' ? _httpUser.text.trim() : null,
          // 留空 = 保持原密码；勾选清除则发空串。
          httpPassword: _kind != 'http'
              ? null
              : _clearPassword
              ? ''
              : (_httpPassword.text.isEmpty ? null : _httpPassword.text),
          routeByHttpUser: _kind == 'http' ? _routeUser.text.trim() : null,
          requestHeaders: _kind == 'http' ? _headerMap : null,
          responseHeaders: _kind == 'http' ? _respHeaderMap : null,
          proxyProtocol: _balanced ? _proxyProtocol : null,
          bandwidthBps: _datagram ? null : _bandwidthBps,
          bandwidthServer: _datagram ? null : _bandwidthServer,
          maxConnections: _maxConnectionCount,
          multiplexer: _kind == 'tcpmux' ? _multiplexer : null,
          useEncryption: _useEncryption,
          useCompression: _useCompression,
          lbGroup: _balanced ? _lbGroup.text.trim() : null,
          // 留空 = 保持原密钥；退出组时一并清掉。
          lbGroupKey: !_balanced
              ? null
              : _lbGroup.text.trim().isEmpty
              ? ''
              : (_lbKey.text.isEmpty ? null : _lbKey.text),
          healthCheck: !_balanced
              ? null
              : (_health ?? const HealthCheck(kind: '')),
          plugin: _pluginOptions,
          reason: '通过面板修改隧道',
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
    final clients =
        ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    final editing = widget.existing != null;
    Widget label(String t) => Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Text(t, style: Type.caption.copyWith(color: c.text3)),
    );
    return DialogShell(
      title: editing ? '编辑隧道' : '新建隧道',
      content: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          if (!editing) ...[
            label('服务模板'),
            LSelect<String>(
              key: const Key('tunnel-template'),
              value: _template,
              options: const [
                LSelectOption('custom', '自定义'),
                LSelectOption('ssh', 'SSH'),
                LSelectOption('rdp', '远程桌面 RDP'),
                LSelectOption('web', 'Web'),
                LSelectOption('mysql', 'MySQL'),
                LSelectOption('postgres', 'PostgreSQL'),
              ],
              onChanged: _busy ? null : _applyTemplate,
            ),
            const SizedBox(height: 12),
          ],
          label('客户端'),
          if (clients.isEmpty || editing)
            Text(
              clients.where((d) => d.id == _client).firstOrNull?.name ?? '—',
              style: Type.body.copyWith(color: c.text1),
            )
          else
            LSelect<String?>(
              value: _client,
              options: [
                if (_client == null)
                  const LSelectOption<String?>(null, '选择客户端'),
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
              key: const Key('tunnel-kind'),
              value: _kind,
              options: const [
                LSelectOption('tcp', 'TCP'),
                LSelectOption('udp', 'UDP'),
                LSelectOption('http', 'HTTP（按域名）'),
                LSelectOption('https', 'HTTPS（按域名，TLS 透传）'),
                LSelectOption('tcpmux', 'TCPMUX（CONNECT 按域名分流）'),
                LSelectOption('stcp', 'STCP（私密 TCP，需访问者）'),
                LSelectOption('sudp', 'SUDP（私密 UDP，需访问者）'),
              ],
              onChanged: (v) => setState(() => _kind = v ?? 'tcp'),
            ),
          const SizedBox(height: 12),
          label('名称'),
          LTextField(
            key: const Key('tunnel-name'),
            controller: _name,
            hint: '例如：ssh',
            onChanged: (_) => setState(() {}),
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
                    label('本地地址'),
                    LTextField(
                      key: const Key('tunnel-local-ip'),
                      controller: _localIp,
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
                    label('本地端口'),
                    LTextField(
                      key: const Key('tunnel-local-port'),
                      controller: _localPort,
                      hint: '22',
                      onChanged: (_) => setState(() {}),
                    ),
                  ],
                ),
              ),
            ],
          ),
          if (_kind == 'tcp' || _kind == 'http') ...[
            const SizedBox(height: 12),
            label('本地插件（可选 JSON）'),
            if (widget.existing?.plugin != null)
              Text(
                '当前已配置 ${widget.existing!.plugin!.kind}。留空保留原配置；填写 {} 可清除，填写完整 JSON 可替换。',
                style: Type.caption.copyWith(color: c.text3),
              )
            else
              Text(
                '支持 socks5、http_proxy、static_file、unix_domain_socket、http2https、https2http、https2https、tls2raw。',
                style: Type.caption.copyWith(color: c.text3),
              ),
            const SizedBox(height: 6),
            LTextField(
              key: const Key('tunnel-plugin-json'),
              controller: _pluginJson,
              hint: '{"kind":"static_file","local_path":"/srv/www"}',
              maxLines: 5,
              onChanged: (_) => setState(() {}),
            ),
            if (!_pluginValid)
              Text(
                '插件 JSON 无效，且插件只支持 TCP 与 HTTP 隧道。',
                style: Type.caption.copyWith(color: c.dangerText),
              ),
          ],
          if (_balanced) ...[
            const SizedBox(height: 12),
            label('PROXY protocol（把访问者真实地址告诉内网服务，服务端需支持）'),
            LSelect<int>(
              key: const Key('tunnel-proxy-protocol'),
              value: _proxyProtocol,
              options: const [
                LSelectOption(0, '不启用'),
                LSelectOption(1, 'v1（文本）'),
                LSelectOption(2, 'v2（二进制）'),
              ],
              onChanged: (v) => setState(() => _proxyProtocol = v ?? 0),
            ),
          ],
          if (!_datagram) ...[
            const SizedBox(height: 12),
            label('限速（KB/s，留空 = 不限；上下行共用）'),
            Row(
              children: [
                Expanded(
                  child: LTextField(
                    key: const Key('tunnel-bandwidth'),
                    controller: _bandwidth,
                    hint: '例如 512',
                    onChanged: (_) => setState(() {}),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: LSelect<bool>(
                    key: const Key('tunnel-bandwidth-mode'),
                    value: _bandwidthServer,
                    options: const [
                      LSelectOption(false, '在客户端限速'),
                      LSelectOption(true, '在服务端限速'),
                    ],
                    onChanged: (v) =>
                        setState(() => _bandwidthServer = v ?? false),
                  ),
                ),
              ],
            ),
          ],
          const SizedBox(height: 12),
          label('最大并发连接 / UDP 活跃源地址数（留空或 0 = 不限）'),
          LTextField(
            key: const Key('tunnel-max-connections'),
            controller: _maxConnections,
            hint: '例如 100',
            onChanged: (_) => setState(() {}),
          ),
          Text(
            'UDP 按活动源地址计数；SUDP 按访问者工作流计数，空闲回收后释放额度。',
            style: Type.caption.copyWith(color: c.text3),
          ),
          if (_maxConnectionCount == null)
            Text(
              '请输入 0 到 1000000 的整数',
              style: Type.caption.copyWith(color: c.dangerText),
            ),
          if (_kind == 'tcpmux') ...[
            const SizedBox(height: 12),
            label('TCPMUX 复用器'),
            LSelect<String>(
              key: const Key('tunnel-multiplexer'),
              value: _multiplexer,
              options: const [LSelectOption('httpconnect', 'HTTP CONNECT')],
              onChanged: (value) =>
                  setState(() => _multiplexer = value ?? 'httpconnect'),
            ),
          ],
          if (['tcp', 'http', 'https', 'tcpmux', 'stcp'].contains(_kind)) ...[
            const SizedBox(height: 8),
            CheckboxListTile(
              key: const Key('tunnel-use-encryption'),
              contentPadding: EdgeInsets.zero,
              dense: true,
              activeColor: c.action,
              value: _useEncryption,
              title: Text('额外加密工作流数据', style: Type.body),
              onChanged: (value) =>
                  setState(() => _useEncryption = value ?? false),
            ),
            CheckboxListTile(
              key: const Key('tunnel-use-compression'),
              contentPadding: EdgeInsets.zero,
              dense: true,
              activeColor: c.action,
              value: _useCompression,
              title: Text('压缩工作流数据', style: Type.body),
              onChanged: (value) =>
                  setState(() => _useCompression = value ?? false),
            ),
          ],
          if (_balanced) ...[
            const SizedBox(height: 12),
            label('负载均衡组（可选；同组隧道共用端口 / 域名，连接在健康成员间轮转）'),
            Row(
              children: [
                Expanded(
                  child: LTextField(
                    key: const Key('tunnel-lb-group'),
                    controller: _lbGroup,
                    hint: '组名',
                    onChanged: (_) => setState(() {}),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: LTextField(
                    key: const Key('tunnel-lb-key'),
                    controller: _lbKey,
                    hint: editing && widget.existing!.lbGroupKeySet
                        ? '组密钥已设置（留空不改）'
                        : '组密钥',
                    obscure: true,
                  ),
                ),
              ],
            ),
            const SizedBox(height: 12),
            label('健康检查（探测本地服务；不健康时暂停接收访问者）'),
            LSelect<String>(
              key: const Key('tunnel-health-kind'),
              value: _hcKind,
              options: const [
                LSelectOption('', '不检查'),
                LSelectOption('tcp', 'TCP 连接'),
                LSelectOption('http', 'HTTP 2xx'),
              ],
              onChanged: (v) => setState(() => _hcKind = v ?? ''),
            ),
            if (_hcKind.isNotEmpty) ...[
              const SizedBox(height: 8),
              if (_hcKind == 'http') ...[
                LTextField(
                  key: const Key('tunnel-health-path'),
                  controller: _hcPath,
                  hint: '/healthz',
                  onChanged: (_) => setState(() {}),
                ),
                const SizedBox(height: 8),
              ],
              Row(
                children: [
                  for (final (key, ctl, hint) in [
                    ('tunnel-health-timeout', _hcTimeout, '超时(秒)'),
                    ('tunnel-health-max', _hcMax, '失败次数'),
                    ('tunnel-health-interval', _hcInterval, '间隔(秒)'),
                  ]) ...[
                    Expanded(
                      child: LTextField(
                        key: Key(key),
                        controller: ctl,
                        hint: hint,
                        onChanged: (_) => setState(() {}),
                      ),
                    ),
                    if (key != 'tunnel-health-interval')
                      const SizedBox(width: 8),
                  ],
                ],
              ),
            ],
          ],
          const SizedBox(height: 12),
          if (_secret)
            Text(
              '私密隧道没有公网端口：在同一节点的其他客户端上添加「访问者」后，才能从那台设备的本机端口访问它。',
              style: Type.caption.copyWith(color: c.text3),
            )
          else if (!_domain) ...[
            label('公网端口（留空 = 自动分配）'),
            LTextField(
              key: const Key('tunnel-remote-port'),
              controller: _remotePort,
              hint: '6000',
              onChanged: (_) => setState(() {}),
            ),
          ] else ...[
            label('自定义域名（多个用逗号分隔，支持 *.example.com）'),
            LTextField(
              key: const Key('tunnel-domains'),
              controller: _domains,
              hint: 'www.example.com',
              onChanged: (_) => setState(() {}),
            ),
            const SizedBox(height: 12),
            label('子域名（需要服务器配置子域名后缀）'),
            LTextField(
              key: const Key('tunnel-subdomain'),
              controller: _sub,
              hint: 'blog',
              onChanged: (_) => setState(() {}),
            ),
            if (_kind == 'http') ...[
              const SizedBox(height: 12),
              label('改写 Host 头（可选）'),
              LTextField(
                key: const Key('tunnel-rewrite'),
                controller: _rewrite,
                hint: 'internal.lan:8080',
              ),
              const SizedBox(height: 12),
              label('路径前缀（逗号分隔，留空 = /；同一域名下可按路径分流）'),
              LTextField(
                key: const Key('tunnel-locations'),
                controller: _locations,
                hint: '/api, /static',
                onChanged: (_) => setState(() {}),
              ),
              const SizedBox(height: 12),
              label('Basic 认证（在 Edge 校验，未通过的请求不会到达内网服务）'),
              Row(
                children: [
                  Expanded(
                    child: LTextField(
                      key: const Key('tunnel-http-user'),
                      controller: _httpUser,
                      hint: '用户名',
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: LTextField(
                      key: const Key('tunnel-http-password'),
                      controller: _httpPassword,
                      hint: editing && widget.existing!.httpPasswordSet
                          ? '已设置，留空保持不变'
                          : '密码',
                      obscure: true,
                      onChanged: (_) => setState(() => _clearPassword = false),
                    ),
                  ),
                ],
              ),
              if (editing && widget.existing!.httpPasswordSet) ...[
                const SizedBox(height: 8),
                Row(
                  children: [
                    LSwitch(
                      key: const Key('tunnel-clear-password'),
                      value: _clearPassword,
                      onChanged: (v) => setState(() {
                        _clearPassword = v;
                        if (v) _httpPassword.clear();
                      }),
                    ),
                    const SizedBox(width: 8),
                    Text('清除密码', style: Type.caption.copyWith(color: c.text3)),
                  ],
                ),
              ],
              const SizedBox(height: 12),
              label('按 Basic 用户名选路（可选，用于同域名同路径的多个后端）'),
              LTextField(
                key: const Key('tunnel-route-user'),
                controller: _routeUser,
                hint: 'alice',
              ),
              const SizedBox(height: 12),
              label('追加 / 覆盖请求头（每行 Name: value）'),
              LTextField(
                key: const Key('tunnel-headers'),
                controller: _headers,
                hint: 'X-Env: prod',
                maxLines: 3,
                height: 72,
                onChanged: (_) => setState(() {}),
              ),
              const SizedBox(height: 12),
              label('追加 / 覆盖响应头（每行 Name: value；不可改 Content-Length 等分帧头）'),
              LTextField(
                key: const Key('tunnel-response-headers'),
                controller: _respHeaders,
                hint: 'X-Frame-Options: deny',
                maxLines: 3,
                height: 72,
                onChanged: (_) => setState(() {}),
              ),
            ],
          ],
        ],
      ),
      actions: [
        LButton(label: '取消', onPressed: () => Navigator.pop(context)),
        LButton(
          key: const Key('tunnel-save'),
          label: '保存',
          variant: BtnVariant.solid,
          loading: _busy,
          onPressed: _valid ? _save : null,
        ),
      ],
    );
  }
}
