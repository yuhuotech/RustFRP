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
  'client.move_node': '迁移服务端节点',
  'client.restart': '重启客户端',
  'node.update': '修改服务端节点',
  'node.delete': '删除服务端节点',
  'node_token.create': '创建节点接入令牌',
  'admin.create': '新增管理员',
  'admin.update': '修改管理员',
  'admin.passwd': '重置管理员密码',
};

String actionLabel(String a) => _actionLabels[a] ?? a;

class AuditView extends ConsumerWidget {
  const AuditView({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final audit = ref.watch(auditProvider);
    return Column(
      children: [
        PageHeader(
          title: '审计日志',
          actions: [
            LIconButton(
              icon: LucideIcons.refreshCw,
              tooltip: '刷新',
              size: 32,
              iconSize: 16,
              onPressed: () => ref.invalidate(auditProvider),
            ),
          ],
        ),
        Expanded(
          child: AsyncView<List<AuditEntry>>(
            value: audit,
            onRetry: () => ref.invalidate(auditProvider),
            builder: (list) => list.isEmpty
                ? const EmptyState(
                    icon: LucideIcons.scrollText,
                    title: '暂无审计记录',
                  )
                : ListView(
                    padding: const EdgeInsets.all(24),
                    children: [
                      PageIntro(
                        icon: LucideIcons.scrollText,
                        title: '操作追踪',
                        description:
                            '核对操作人、来源 IP、目标资源和操作理由。此处展示当前返回的记录，刷新可获取最新结果。',
                        color: c.warningText,
                        badges: [LBadge('本次加载 ${list.length} 条')],
                      ),
                      ResourceTable(
                        minWidth: 1000,
                        columns: const [
                          ('时间', 3),
                          ('操作 / 标识', 3),
                          ('操作人 / 来源 IP', 3),
                          ('目标资源', 3),
                          ('理由', 4),
                          ('结果', 2),
                        ],
                        rows: [
                          for (final e in list)
                            ResourceRow(
                              key: Key('audit-${e.id}'),
                              cells: [
                                (TableText(fullTime(e.ts), mono: true), 3),
                                (
                                  TableText(
                                    actionLabel(e.action),
                                    subtitle: actionLabel(e.action) == e.action
                                        ? null
                                        : e.action,
                                    color: c.directText,
                                  ),
                                  3,
                                ),
                                (
                                  TableText(
                                    e.actor,
                                    subtitle: e.ip.isEmpty ? '无来源 IP' : e.ip,
                                  ),
                                  3,
                                ),
                                (
                                  TableText(
                                    e.targetId.isEmpty
                                        ? '—'
                                        : '${e.targetKind} ${e.targetId}',
                                    mono: true,
                                  ),
                                  3,
                                ),
                                (
                                  TableText(
                                    e.reason.isEmpty ? '未填写' : '理由：${e.reason}',
                                  ),
                                  4,
                                ),
                                (
                                  Align(
                                    alignment: Alignment.centerLeft,
                                    child: LBadge(
                                      e.result == 'ok'
                                          ? '成功'
                                          : (e.result == 'denied'
                                                ? '拒绝'
                                                : e.result),
                                      bg: e.result == 'ok'
                                          ? c.successSoft
                                          : c.dangerSoft,
                                      fg: e.result == 'ok'
                                          ? c.successText
                                          : c.dangerText,
                                    ),
                                  ),
                                  2,
                                ),
                              ],
                            ),
                        ],
                      ),
                    ],
                  ),
          ),
        ),
      ],
    );
  }
}
