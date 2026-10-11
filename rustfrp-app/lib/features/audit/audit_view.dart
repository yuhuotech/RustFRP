import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';

const _actionLabels = {
  'auth.login': '登录',
  'client.enroll': '客户端接入',
  'client.update': '修改客户端',
  'client.kick': '强制下线',
  'client.delete': '删除客户端',
  'tunnel.create': '创建隧道',
  'tunnel.update': '修改隧道',
  'tunnel.delete': '删除隧道',
  'enroll_token.create': '创建接入令牌',
  'enroll_token.revoke': '撤销接入令牌',
  'permission.denied': '权限不足',
};

String actionLabel(String a) => _actionLabels[a] ?? a;

class AuditView extends ConsumerWidget {
  const AuditView({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final audit = ref.watch(auditProvider);
    return Column(children: [
      PageHeader(title: '审计', actions: [
        LIconButton(icon: LucideIcons.refreshCw, tooltip: '刷新', size: 32, iconSize: 16, onPressed: () => ref.invalidate(auditProvider)),
      ]),
      Expanded(
        child: AsyncView<List<AuditEntry>>(
          value: audit,
          onRetry: () => ref.invalidate(auditProvider),
          builder: (list) => list.isEmpty
              ? const EmptyState(icon: LucideIcons.scrollText, title: '暂无审计记录')
              : ListView(padding: const EdgeInsets.all(24), children: [
                  for (final e in list)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 6),
                      child: PanelCard(
                        key: Key('audit-${e.id}'),
                        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                          Row(children: [
                            Text(actionLabel(e.action), style: Type.strong.copyWith(color: c.text1)),
                            const SizedBox(width: 8),
                            LBadge(e.result == 'ok' ? '成功' : '拒绝', bg: e.result == 'ok' ? c.successSoft : c.dangerSoft, fg: e.result == 'ok' ? c.successText : c.dangerText),
                            const Spacer(),
                            Text(fullTime(e.ts), style: Type.caption.copyWith(color: c.text3)),
                          ]),
                          const SizedBox(height: 4),
                          Text('${e.actor}${e.ip.isEmpty ? '' : ' · ${e.ip}'}${e.targetId.isEmpty ? '' : ' · ${e.targetKind} ${e.targetId}'}', style: Type.caption.copyWith(color: c.text2)),
                          if (e.reason.isNotEmpty) Text('理由：${e.reason}', style: Type.caption.copyWith(color: c.text3)),
                        ]),
                      ),
                    ),
                ]),
        ),
      ),
    ]);
  }
}
