import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../shared/workspace_widgets.dart';
import '../../theme/tokens.dart';

class ApiTokensView extends ConsumerWidget {
  const ApiTokensView({super.key});
  @override
  Widget build(BuildContext context, WidgetRef ref) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      LButton(
        key: const Key('new-api-token'),
        label: '创建 API Token',
        onPressed: () => showDialog<void>(
          context: context,
          builder: (_) => const _CreateToken(),
        ),
      ),
      const SizedBox(height: 12),
      Text(
        '自动化访问使用 Bearer Token。原文只显示一次；请安全保存，不要写入日志或代码仓库。',
        style: Type.caption.copyWith(color: context.c.text3),
      ),
      const SizedBox(height: 16),
      AsyncView<List<Map<String, dynamic>>>(
        value: ref.watch(apiTokensProvider),
        onRetry: () => ref.invalidate(apiTokensProvider),
        builder: (items) => items.isEmpty
            ? const Text('暂无 API Token')
            : ResourceTable(
                columns: const [
                  ('令牌名称', 3),
                  ('权限范围', 2),
                  ('到期时间', 3),
                  ('状态', 2),
                  ('操作', 2),
                ],
                rows: [
                  for (final token in items)
                    ResourceRow(
                      cells: [
                        (TableText(token['name'] as String), 3),
                        (
                          TableText(
                            token['scope'] as String,
                            color: context.c.directText,
                          ),
                          2,
                        ),
                        (
                          TableText(
                            '${fullTime(token['expires_at'] as int)} 到期',
                            mono: true,
                          ),
                          3,
                        ),
                        (
                          Align(
                            alignment: Alignment.centerLeft,
                            child: LBadge(
                              token['revoked_at'] != null
                                  ? '已撤销'
                                  : ((token['expires_at'] as int) <=
                                            DateTime.now()
                                                .millisecondsSinceEpoch
                                        ? '已过期'
                                        : '有效'),
                              bg: token['revoked_at'] != null
                                  ? context.c.bgSubtle
                                  : context.c.warningSoft,
                              fg: token['revoked_at'] != null
                                  ? context.c.text2
                                  : context.c.warningText,
                            ),
                          ),
                          2,
                        ),
                        (
                          token['revoked_at'] == null
                              ? LButton(
                                  key: Key('revoke-api-token-${token['id']}'),
                                  label: '撤销',
                                  onPressed: () async {
                                    final why = await askReason(
                                      context,
                                      title: '撤销 API Token',
                                      message: '撤销后，使用这个令牌的自动化调用会立即失效。',
                                      confirm: '撤销',
                                      destructive: true,
                                    );
                                    if (why == null || !context.mounted) return;
                                    await guarded(context, () async {
                                      await ref
                                          .read(apiProvider)
                                          .revokeApiToken(
                                            token['id'] as String,
                                            why,
                                          );
                                      ref.invalidate(apiTokensProvider);
                                    });
                                  },
                                )
                              : const Text('—'),
                          2,
                        ),
                      ],
                    ),
                ],
              ),
      ),
    ],
  );
}

class _CreateToken extends ConsumerStatefulWidget {
  const _CreateToken();
  @override
  ConsumerState<_CreateToken> createState() => _CreateTokenState();
}

class _CreateTokenState extends ConsumerState<_CreateToken> {
  final name = TextEditingController();
  String scope = 'read';
  int days = 30;
  bool busy = false;
  String? token;
  @override
  void dispose() {
    name.dispose();
    super.dispose();
  }

  Future<void> create() async {
    if (busy) return;
    setState(() => busy = true);
    final result = await guarded(
      context,
      () => ref
          .read(apiProvider)
          .createApiToken(name.text.trim(), scope, days, '通过面板创建自动化令牌'),
    );
    if (!mounted) return;
    setState(() {
      busy = false;
      token = result?['token'] as String?;
    });
    ref.invalidate(apiTokensProvider);
  }

  @override
  Widget build(BuildContext context) => DialogShell(
    title: token == null ? '创建 API Token' : '保存 API Token',
    content: token != null
        ? Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              CopyField(text: token!),
              const SizedBox(height: 12),
              const Text('关闭后无法再次查看原文。'),
            ],
          )
        : Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              LTextField(
                key: const Key('api-token-name'),
                controller: name,
                hint: '例如：CI 部署',
                onChanged: (_) => setState(() {}),
              ),
              const SizedBox(height: 12),
              LSelect<String>(
                key: const Key('api-token-scope'),
                value: scope,
                options: const [
                  LSelectOption('read', '只读'),
                  LSelectOption('tunnels', '隧道管理'),
                  LSelectOption('admin', '设备与隧道管理'),
                ],
                onChanged: busy
                    ? null
                    : (v) => setState(() => scope = v ?? 'read'),
              ),
              const SizedBox(height: 12),
              LSelect<int>(
                value: days,
                options: const [
                  LSelectOption(1, '1 天'),
                  LSelectOption(30, '30 天'),
                  LSelectOption(90, '90 天'),
                  LSelectOption(365, '365 天'),
                ],
                onChanged: busy ? null : (v) => setState(() => days = v ?? 30),
              ),
            ],
          ),
    actions: [
      LButton(
        label: '关闭',
        onPressed: busy ? null : () => Navigator.pop(context),
      ),
      if (token == null)
        LButton(
          key: const Key('create-api-token'),
          label: '创建',
          loading: busy,
          onPressed: name.text.trim().isEmpty || busy ? null : create,
        ),
    ],
  );
}
