import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';
import '../clients/enroll_dialog.dart';
import 'traffic_panel.dart';

class OverviewView extends ConsumerWidget {
  const OverviewView({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final overview = ref.watch(overviewProvider);
    final node = ref.watch(nodeProvider);
    final clients = ref.watch(clientsProvider);
    return Column(
      children: [
        PageHeader(
          title: '概览',
          actions: [
            if (role.atLeast(Role.admin))
              LButton(
                label: '接入新客户端',
                icon: LucideIcons.plus,
                variant: BtnVariant.solid,
                onPressed: () => showEnrollDialog(context),
              ),
          ],
        ),
        Expanded(
          child: AsyncView<Overview>(
            value: overview,
            onRetry: () => ref.invalidate(overviewProvider),
            builder: (o) => ListView(
              padding: const EdgeInsets.all(24),
              children: [
                LayoutBuilder(
                  builder: (context, box) {
                    final cols = box.maxWidth >= 800 ? 3 : 2;
                    final w = (box.maxWidth - (cols - 1) * 12) / cols;
                    Widget m(Widget child) => SizedBox(width: w, child: child);
                    return Wrap(
                      spacing: 12,
                      runSpacing: 12,
                      children: [
                        m(
                          MetricCard(
                            key: const Key('metric-clients'),
                            label: '在线客户端',
                            value: '${o.clientsOnline}',
                            hint: '共 ${o.clients} 台',
                            icon: LucideIcons.monitor,
                          ),
                        ),
                        m(
                          MetricCard(
                            key: const Key('metric-tunnels'),
                            label: '运行中的隧道',
                            value: '${o.tunnelsRunning}',
                            hint: '共 ${o.tunnels} 条',
                            icon: LucideIcons.waypoints,
                          ),
                        ),
                        m(
                          MetricCard(
                            key: const Key('metric-tokens'),
                            label: '待使用的接入令牌',
                            value: '${o.tokensPending}',
                            icon: LucideIcons.keyRound,
                          ),
                        ),
                        m(
                          MetricCard(
                            label: '当前连接',
                            value: '${o.connections}',
                            icon: LucideIcons.activity,
                          ),
                        ),
                        m(
                          MetricCard(
                            key: const Key('metric-traffic-24h'),
                            label: '24 小时流量',
                            value: formatBytes(o.bytes24hIn + o.bytes24hOut),
                            hint:
                                '↓ ${formatBytes(o.bytes24hIn)} · ↑ ${formatBytes(o.bytes24hOut)}',
                            icon: LucideIcons.chartNoAxesColumn,
                          ),
                        ),
                        SizedBox(
                          width: box.maxWidth,
                          child: MetricCard(
                            key: const Key('metric-controller'),
                            label: '控制器版本',
                            value: o.version.isEmpty ? '—' : o.version,
                            hint: [
                              if (o.rssBytes != null)
                                '内存 ${formatBytes(o.rssBytes!)}',
                              '运行 ${formatDuration(o.uptimeSeconds)}',
                              if (o.cpuSeconds != null)
                                'CPU 累计 ${o.cpuSeconds!.toStringAsFixed(1)}s',
                            ].join(' · '),
                            icon: LucideIcons.server,
                          ),
                        ),
                      ],
                    );
                  },
                ),
                const SizedBox(height: 24),
                const TrafficPanel(),
                const SizedBox(height: 24),
                const SectionTitle('节点'),
                node.maybeWhen(
                  data: (n) => !n.enabled
                      ? const SizedBox.shrink()
                      : PanelCard(
                          child: Column(
                            children: [
                              InfoRow('名称', '内置节点（${n.name}）'),
                              InfoRow(
                                '公网地址',
                                n.publicHost.isEmpty
                                    ? '未配置'
                                    : hostPort(n.publicHost, n.edgePort),
                                mono: true,
                              ),
                              InfoRow('可用端口', n.allowPortsLabel),
                            ],
                          ),
                        ),
                  orElse: () => const SizedBox.shrink(),
                ),
                const SizedBox(height: 24),
                const SectionTitle('最近的客户端'),
                clients.maybeWhen(
                  data: (list) => list.isEmpty
                      ? PanelCard(
                          child: Text(
                            '还没有客户端。点击右上角「接入新客户端」，把命令粘贴到目标机器上执行即可。',
                            style: Type.body.copyWith(color: c.text3),
                          ),
                        )
                      : Column(
                          children: [
                            for (final d in list.take(5))
                              Padding(
                                padding: const EdgeInsets.only(bottom: 8),
                                child: PanelCard(
                                  child: Row(
                                    children: [
                                      DeviceGlyph(
                                        type: d.os,
                                        size: 32,
                                        online: d.online,
                                      ),
                                      const SizedBox(width: 12),
                                      Expanded(
                                        child: Text(
                                          d.name,
                                          style: Type.strong.copyWith(
                                            color: c.text1,
                                          ),
                                          overflow: TextOverflow.ellipsis,
                                        ),
                                      ),
                                      const SizedBox(width: 8),
                                      Flexible(
                                        child: Text(
                                          d.online
                                              ? '在线'
                                              : '最近在线 ${relativeTime(d.lastSeenAt)}',
                                          style: Type.caption.copyWith(
                                            color: c.text3,
                                          ),
                                          overflow: TextOverflow.ellipsis,
                                        ),
                                      ),
                                    ],
                                  ),
                                ),
                              ),
                          ],
                        ),
                  orElse: () => const SizedBox.shrink(),
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}
