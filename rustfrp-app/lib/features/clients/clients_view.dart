import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
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
              if (d.online) ...[
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
              const SectionTitle('设备信息'),
              PanelCard(
                child: Column(
                  children: [
                    InfoRow('设备 ID', d.id, mono: true),
                    InfoRow('系统', '${osLabel(d.os)} ${d.arch}'),
                    InfoRow('Agent 版本', d.version.isEmpty ? '—' : d.version),
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
                    InfoRow('配置版本', '${d.revision}'),
                  ],
                ),
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
