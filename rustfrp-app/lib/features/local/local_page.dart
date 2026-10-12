import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/local_agent.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/format.dart';
import '../../shared/widgets.dart';
import '../../shared/sidebar.dart';
import '../../theme/tokens.dart';

class LocalPage extends ConsumerStatefulWidget {
  const LocalPage({super.key});
  @override
  ConsumerState<LocalPage> createState() => _LocalPageState();
}

class _LocalPageState extends ConsumerState<LocalPage> {
  late final Map<String, TextEditingController> _fields;
  final _token = TextEditingController();
  final _scroll = ScrollController();
  bool _standaloneMode = false;
  bool _exporting = false;
  String? _diagnosticResult;
  Future<void> _exportDiagnostic() async {
    if (_exporting) return;
    setState(() {
      _exporting = true;
      _diagnosticResult = null;
    });
    try {
      final path = await ref
          .read(localBackendProvider)
          .exportDiagnostics(ref.read(localAgentProvider).config);
      if (mounted) setState(() => _diagnosticResult = '诊断文件已保存：$path');
    } catch (e) {
      if (mounted) setState(() => _diagnosticResult = '导出失败：$e');
    } finally {
      if (mounted) setState(() => _exporting = false);
    }
  }

  Future<void> _editStandaloneConfig() async {
    final path = _fields['configFile']?.text.trim() ?? '';
    if (path.isEmpty) {
      setState(() => _diagnosticResult = '请先填写 frpc 配置文件的绝对路径。');
      return;
    }
    final current = ref.read(localAgentProvider).config;
    final config = LocalAgentConfig.fromJson({
      ...current.toJson(),
      'configFile': path,
    });
    try {
      final text = await ref.read(localBackendProvider).readConfig(config);
      if (!mounted) return;
      final saved = await showDialog<bool>(
        context: context,
        builder: (_) => StandaloneConfigEditor(config: config, initialText: text),
      );
      if (saved == true && mounted) {
        setState(() => _diagnosticResult = '配置已通过 Agent verify 并原子保存。启动独立 Agent 后生效。');
      }
    } on Object catch (error) {
      if (mounted) setState(() => _diagnosticResult = '无法打开配置文件：$error');
    }
  }

  void _goTop() {
    _scroll.animateTo(
      0,
      duration: const Duration(milliseconds: 150),
      curve: Curves.easeOut,
    );
  }

  @override
  void initState() {
    super.initState();
    final config = ref.read(localAgentProvider).config;
    _standaloneMode = config.configFile.isNotEmpty;
    _fields = {
      for (final e in config.toJson().entries)
        e.key: TextEditingController(text: e.value as String),
    };
    Future.microtask(() => ref.read(localAgentProvider.notifier).refresh());
  }

  @override
  void dispose() {
    for (final c in _fields.values) {
      c.dispose();
    }
    _token.dispose();
    _scroll.dispose();
    super.dispose();
  }

  Future<void> _start() async {
    final fields = {
      for (final e in _fields.entries) e.key: e.value.text.trim(),
    };
    fields['controller'] = _standaloneMode ? '' : fields['controller']!;
    fields['configFile'] = _standaloneMode ? fields['configFile']! : '';
    final config = LocalAgentConfig.fromJson(fields);
    final token = _token.text;
    _token.clear();
    await ref.read(localAgentProvider.notifier).start(config, token);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final state = ref.watch(localAgentProvider);
    final status = state.status;
    final label = state.error != null && status != null
        ? '状态暂不可用'
        : (status == null ? '未运行' : (status.connected ? '已连接' : '连接中 / 重连中'));
    final color = state.error != null
        ? c.warning
        : (status == null
              ? c.controlOff
              : (status.connected ? c.success : c.warning));
    final content = Column(
      children: [
        PageHeader(
          title: '本机模式',
          actions: [
            LButton(
              key: const Key('local-refresh'),
              label: '刷新',
              icon: LucideIcons.refreshCw,
              onPressed: state.busy
                  ? null
                  : () => ref.read(localAgentProvider.notifier).refresh(),
            ),
            LButton(
              key: const Key('admin-mode'),
              label: '管理员控制台',
              onPressed: () =>
                  ref.read(localModeProvider.notifier).select(false),
            ),
          ],
        ),
        Expanded(
          child: ListView(
            controller: _scroll,
            padding: const EdgeInsets.all(24),
            children: [
              Row(
                children: [
                  StatusDot(color: color),
                  const SizedBox(width: 8),
                  Text(
                    label,
                    key: const Key('local-status'),
                    style: Type.strong.copyWith(color: c.text1),
                  ),
                ],
              ),
              const SizedBox(height: 16),
              if (status != null) ...[
                PanelCard(
                  child: Column(
                    children: [
                      InfoRow('进程', '${status.pid}'),
                      InfoRow('已应用隧道', '${status.proxies}'),
                      InfoRow('配置版本', '${status.revision}'),
                      if (status.bytesIn != null) ...[
                        InfoRow('当前连接', '${status.connections}'),
                        InfoRow('本机入站', formatBytes(status.bytesIn!)),
                        InfoRow('本机出站', formatBytes(status.bytesOut!)),
                        InfoRow('统计范围', '本次运行累计，重启后归零'),
                      ] else
                        const InfoRow('流量统计', '请更新 agent 以查看本机统计'),
                    ],
                  ),
                ),
                const SizedBox(height: 16),
                LButton(
                  key: const Key('local-tunnels'),
                  label: '查看已应用隧道',
                  onPressed: () => showDialog<void>(
                    context: context,
                    builder: (_) => const _LocalTunnelsDialog(),
                  ),
                ),
                const SizedBox(height: 12),
                LButton(
                  key: const Key('local-logs'),
                  label: '查看本机日志',
                  onPressed: () => showDialog<void>(
                    context: context,
                    builder: (_) => const _LocalLogsDialog(),
                  ),
                ),
                const SizedBox(height: 16),
                LButton(
                  key: const Key('local-diagnostics'),
                  label: '导出诊断快照',
                  loading: _exporting,
                  onPressed: _exporting ? null : _exportDiagnostic,
                ),
                if (_diagnosticResult != null) ...[
                  const SizedBox(height: 8),
                  SelectableText(
                    _diagnosticResult!,
                    key: const Key('local-diagnostic-result'),
                    style: Type.caption.copyWith(color: c.text2),
                  ),
                ],
                const SizedBox(height: 16),
                Text(
                  '隧道配置由控制器下发。退出界面不会停止 agent；需要断开时请点击「停止」。',
                  style: Type.body.copyWith(color: c.text2),
                ),
                const SizedBox(height: 16),
              ],
              const SectionTitle('连接设置'),
              // Running profiles are immutable until explicitly stopped.
              if (status == null) ...[
                Wrap(
                  spacing: 8,
                  children: [
                    LButton(
                      key: const Key('local-managed-mode'),
                      label: '受管模式',
                      compact: true,
                      variant: _standaloneMode
                          ? BtnVariant.neutral
                          : BtnVariant.solid,
                      onPressed: () => setState(() => _standaloneMode = false),
                    ),
                    LButton(
                      key: const Key('local-standalone-mode'),
                      label: '独立模式',
                      compact: true,
                      variant: _standaloneMode
                          ? BtnVariant.solid
                          : BtnVariant.neutral,
                      onPressed: () => setState(() => _standaloneMode = true),
                    ),
                  ],
                ),
                const SizedBox(height: 16),
                for (final field in [
                  ('binary', 'Agent 路径', '留空采用安装包内置 Agent'),
                  if (_standaloneMode)
                    ('configFile', 'frpc 配置文件', '/绝对路径/frpc.toml')
                  else ...[
                    ('controller', 'Edge 地址', 'frp.example.com:7000'),
                    ('name', '设备名称', '家里的 NAS'),
                    ('stateDir', '状态目录', '/绝对路径/状态目录'),
                    ('caFile', 'CA 证书（可选）', '/绝对路径/ca.pem'),
                    ('tlsPin', '证书指纹（来自可信接入命令）', '64 位 SHA256，可与 CA 同时使用'),
                  ],
                  if (_standaloneMode) ('stateDir', '状态目录', '/绝对路径/状态目录'),
                ]) ...[
                  Text(field.$2, style: Type.caption.copyWith(color: c.text3)),
                  const SizedBox(height: 4),
                  LTextField(
                    key: Key('local-${field.$1}'),
                    controller: _fields[field.$1],
                    hint: field.$3,
                  ),
                  const SizedBox(height: 12),
                ],
                if (_standaloneMode)
                  Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        '独立模式直接读取本机 frpc 配置，不连接 Controller。保存前会运行 Agent verify。',
                        style: Type.caption.copyWith(color: c.text3),
                      ),
                      const SizedBox(height: 8),
                      LButton(
                        key: const Key('local-edit-config'),
                        label: '编辑 frpc 配置',
                        icon: LucideIcons.filePenLine,
                        onPressed: state.status == null ? _editStandaloneConfig : null,
                      ),
                    ],
                  )
                else ...[
                  Text(
                    '一次性接入令牌（首次接入填写）',
                    style: Type.caption.copyWith(color: c.text3),
                  ),
                  const SizedBox(height: 4),
                  LTextField(
                    key: const Key('local-token'),
                    controller: _token,
                    obscure: true,
                  ),
                  const SizedBox(height: 12),
                  Text(
                    '令牌不会保存。已接入设备复用状态目录中的身份；不同控制器请使用不同的状态目录。',
                    style: Type.caption.copyWith(color: c.text3),
                  ),
                ],
              ] else
                PanelCard(
                  child: Column(
                    children: [
                      InfoRow(
                        state.config.configFile.isEmpty ? 'Edge' : '配置文件',
                        state.config.configFile.isEmpty
                            ? state.config.controller
                            : state.config.configFile,
                      ),
                      InfoRow('设备名称', state.config.name),
                      InfoRow('Agent 路径', state.config.binary, mono: true),
                      InfoRow('状态目录', state.config.stateDir, mono: true),
                    ],
                  ),
                ),
              if (state.error != null) ...[
                const SizedBox(height: 12),
                Text(
                  state.error!,
                  key: const Key('local-error'),
                  style: Type.body.copyWith(color: c.dangerText),
                ),
              ],
              const SizedBox(height: 20),
              Align(
                alignment: Alignment.centerLeft,
                child: LButton(
                  key: Key(status == null ? 'local-start' : 'local-stop'),
                  label: status == null
                      ? (_standaloneMode ? '启动独立客户端' : '启动并接入')
                      : '停止',
                  icon: status == null ? LucideIcons.play : LucideIcons.square,
                  variant: status == null
                      ? BtnVariant.solid
                      : BtnVariant.neutral,
                  loading: state.busy,
                  onPressed: status == null
                      ? _start
                      : () => ref.read(localAgentProvider.notifier).stop(),
                ),
              ),
            ],
          ),
        ),
      ],
    );
    return Scaffold(
      backgroundColor: c.bgApp,
      body: SafeArea(
        child: isNarrow(context)
            ? content
            : Row(
                children: [
                  AppSidebar(
                    items: [
                      SidebarEntry(
                        label: '本机模式',
                        icon: LucideIcons.monitor,
                        selected: true,
                        onSelect: () {},
                      ),
                    ],
                    bottomItems: [
                      SidebarEntry(
                        label: '管理员控制台',
                        icon: LucideIcons.shieldCheck,
                        onSelect: () =>
                            ref.read(localModeProvider.notifier).select(false),
                      ),
                    ],
                    onHome: _goTop,
                  ),
                  Container(width: 1, color: c.border),
                  Expanded(child: content),
                ],
              ),
      ),
    );
  }
}

class StandaloneConfigEditor extends ConsumerStatefulWidget {
  const StandaloneConfigEditor({
    super.key,
    required this.config,
    required this.initialText,
  });
  final LocalAgentConfig config;
  final String initialText;

  @override
  ConsumerState<StandaloneConfigEditor> createState() =>
      _StandaloneConfigEditorState();
}

class _StandaloneConfigEditorState extends ConsumerState<StandaloneConfigEditor> {
  late final _text = TextEditingController(text: widget.initialText);
  bool _saving = false;
  String? _error;

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    if (_saving) return;
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      await ref.read(localBackendProvider).saveConfig(widget.config, _text.text);
      if (mounted) Navigator.pop(context, true);
    } on Object catch (error) {
      if (mounted) setState(() => _error = error.toString());
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  @override
  Widget build(BuildContext context) => DialogShell(
    title: '编辑本机 frpc 配置',
    maxWidth: 900,
    content: Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(widget.config.configFile, style: Type.caption.copyWith(color: context.c.text3)),
        const SizedBox(height: 8),
        SizedBox(
          height: 440,
          child: LTextField(
            key: const Key('standalone-config-text'),
            controller: _text,
            maxLines: null,
            hint: '粘贴或编辑完整 frpc TOML / YAML / JSON 配置',
            onChanged: (_) => setState(() => _error = null),
          ),
        ),
        if (_error != null) ...[
          const SizedBox(height: 8),
          SelectableText(_error!, style: Type.caption.copyWith(color: context.c.dangerText)),
        ],
      ],
    ),
    actions: [
      LButton(label: '取消', onPressed: _saving ? null : () => Navigator.pop(context)),
      LButton(
        key: const Key('standalone-config-save'),
        label: '校验并保存',
        icon: LucideIcons.save,
        variant: BtnVariant.solid,
        loading: _saving,
        onPressed: _saving ? null : _save,
      ),
    ],
  );
}

class _LocalLogsDialog extends ConsumerWidget {
  const _LocalLogsDialog();
  @override
  Widget build(BuildContext context, WidgetRef ref) => DialogShell(
    title: '本机日志',
    content: SizedBox(
      height: 300,
      width: 640,
      child: AsyncView<List<String>>(
        value: ref.watch(localLogsProvider),
        onRetry: () => ref.invalidate(localLogsProvider),
        builder: (lines) => lines.isEmpty
            ? const Center(child: Text('暂无日志'))
            : ListView.builder(
                itemCount: lines.length,
                itemBuilder: (context, index) => Padding(
                  padding: const EdgeInsets.only(bottom: 6),
                  child: SelectableText(
                    lines[index],
                    style: Type.body.copyWith(color: context.c.text2),
                  ),
                ),
              ),
      ),
    ),
    actions: [
      LButton(
        key: const Key('local-logs-refresh'),
        label: '刷新',
        onPressed: () => ref.invalidate(localLogsProvider),
      ),
      LButton(
        key: const Key('local-logs-close'),
        label: '关闭',
        onPressed: () => Navigator.pop(context),
      ),
    ],
  );
}

class _LocalTunnelsDialog extends ConsumerWidget {
  const _LocalTunnelsDialog();
  @override
  Widget build(BuildContext context, WidgetRef ref) => DialogShell(
    title: '已应用隧道',
    maxWidth: 640,
    content: SizedBox(
      height: 320,
      child: AsyncView<List<Map<String, dynamic>>>(
        value: ref.watch(localTunnelsProvider),
        onRetry: () => ref.invalidate(localTunnelsProvider),
        builder: (items) => items.isEmpty
            ? const Center(child: Text('当前没有已应用隧道'))
            : ListView.builder(
                itemCount: items.length,
                itemBuilder: (context, index) {
                  final t = items[index];
                  final port = t['remote_port'] as int;
                  return Padding(
                    padding: const EdgeInsets.only(bottom: 12),
                    child: PanelCard(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          SelectableText(
                            '${t['kind'].toString().toUpperCase()} · ${t['id']}',
                            style: Type.strong.copyWith(color: context.c.text1),
                          ),
                          const SizedBox(height: 6),
                          SelectableText(
                            '本地目标 ${hostPort(t['local_ip'] as String, t['local_port'] as int)}',
                            style: Type.body.copyWith(color: context.c.text2),
                          ),
                          const SizedBox(height: 4),
                          Text(
                            port == 0 ? '公网地址尚未确认' : '配置的远程端口 $port',
                            style: Type.caption.copyWith(
                              color: context.c.text3,
                            ),
                          ),
                        ],
                      ),
                    ),
                  );
                },
              ),
      ),
    ),
    actions: [
      LButton(
        key: const Key('local-tunnels-refresh'),
        label: '刷新',
        onPressed: () => ref.invalidate(localTunnelsProvider),
      ),
      LButton(
        key: const Key('local-tunnels-close'),
        label: '关闭',
        onPressed: () => Navigator.pop(context),
      ),
    ],
  );
}
