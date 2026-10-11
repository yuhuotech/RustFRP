import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/format.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';

class TrafficPanel extends ConsumerStatefulWidget {
  const TrafficPanel({super.key});
  @override
  ConsumerState<TrafficPanel> createState() => _TrafficPanelState();
}

class _TrafficPanelState extends ConsumerState<TrafficPanel> {
  int hours = 24;
  String? clientId, tunnelId;
  @override
  Widget build(BuildContext context) {
    final clients =
        ref.watch(clientsProvider).asData?.value ?? const <ClientDevice>[];
    final tunnels =
        ref.watch(tunnelsProvider).asData?.value ?? const <Tunnel>[];
    final selectedClient = clients.any((c) => c.id == clientId)
        ? clientId
        : null;
    final available = tunnels
        .where((t) => selectedClient == null || t.clientId == selectedClient)
        .toList();
    final selectedTunnel = available.any((t) => t.id == tunnelId)
        ? tunnelId
        : null;
    final filter = (
      hours: hours,
      clientId: selectedClient,
      tunnelId: selectedTunnel,
    );
    final result = ref.watch(trafficProvider(filter));
    return PanelCard(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Wrap(
            spacing: 8,
            runSpacing: 8,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              Text('流量历史', style: Type.strong.copyWith(color: context.c.text1)),
              for (final range in [24, 168])
                LButton(
                  key: Key('traffic-range-$range'),
                  label: range == 24 ? '24 小时' : '7 天',
                  variant: hours == range ? BtnVariant.solid : BtnVariant.ghost,
                  onPressed: () => setState(() => hours = range),
                ),
              SizedBox(
                width: 150,
                child: LSelect<String?>(
                  key: const Key('traffic-client'),
                  value: selectedClient,
                  options: [
                    const LSelectOption(null, '全部客户端'),
                    for (final c in clients) LSelectOption(c.id, c.name),
                  ],
                  onChanged: (v) => setState(() {
                    clientId = v;
                    tunnelId = null;
                  }),
                ),
              ),
              SizedBox(
                width: 150,
                child: LSelect<String?>(
                  key: const Key('traffic-tunnel'),
                  value: selectedTunnel,
                  options: [
                    const LSelectOption(null, '全部隧道'),
                    for (final t in available) LSelectOption(t.id, t.name),
                  ],
                  onChanged: (v) => setState(() => tunnelId = v),
                ),
              ),
            ],
          ),
          const SizedBox(height: 16),
          if (result.hasError && result.hasValue) ...[
            Wrap(
              spacing: 12,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                Text(
                  '刷新失败，显示上次采样结果',
                  key: const Key('traffic-stale'),
                  style: Type.caption.copyWith(color: context.c.warningText),
                ),
                LButton(
                  label: '重试',
                  onPressed: result.isLoading
                      ? null
                      : () => ref.invalidate(trafficProvider(filter)),
                ),
              ],
            ),
            const SizedBox(height: 8),
          ],
          AsyncView<TrafficHistory>(
            value: result,
            onRetry: () => ref.invalidate(trafficProvider(filter)),
            builder: (history) =>
                TrafficChart(key: ValueKey(filter), history: history),
          ),
          const SizedBox(height: 12),
          Text(
            '按小时汇总已观测流量；空白表示无采样，虚线表示观测不完整。约每 5 秒采样，时间按本机时区显示。',
            style: Type.caption.copyWith(color: context.c.text3),
          ),
        ],
      ),
    );
  }
}

class TrafficChart extends StatefulWidget {
  const TrafficChart({super.key, required this.history});
  final TrafficHistory history;
  @override
  State<TrafficChart> createState() => _TrafficChartState();
}

class _TrafficChartState extends State<TrafficChart> {
  int? selected;
  @override
  Widget build(BuildContext context) {
    final history = widget.history;
    final slots = history.slots;
    final index = selected?.clamp(0, slots.length - 1);
    final bucket = index == null ? null : slots[index];
    final c = context.c;
    if (history.items.isEmpty) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 30),
        child: Text('暂无流量采样', style: Type.body.copyWith(color: c.text3)),
      );
    }
    final maxBytes = history.items.fold<int>(
      1,
      (v, b) => math.max(v, math.max(b.bytesIn, b.bytesOut)),
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Wrap(
          spacing: 20,
          runSpacing: 8,
          children: [
            Text(
              '入站 ${formatBytes(history.bytesIn)}',
              style: Type.body.copyWith(color: c.actionText),
            ),
            Text(
              '出站 ${formatBytes(history.bytesOut)}',
              style: Type.body.copyWith(color: c.directText),
            ),
            Text(
              '每小时 · 纵轴上限 ${formatBytes(maxBytes)}',
              style: Type.caption.copyWith(color: c.text3),
            ),
          ],
        ),
        const SizedBox(height: 12),
        LayoutBuilder(
          builder: (context, box) {
            void choose(double x) => setState(
              () => selected = (x / box.maxWidth * slots.length).floor().clamp(
                0,
                slots.length - 1,
              ),
            );
            return Semantics(
              label: '入站与出站小时流量图。点击或悬停查看小时数据',
              child: MouseRegion(
                onHover: (e) => choose(e.localPosition.dx),
                child: GestureDetector(
                  key: const Key('traffic-chart'),
                  behavior: HitTestBehavior.opaque,
                  onTapDown: (e) => choose(e.localPosition.dx),
                  child: SizedBox(
                    height: 140,
                    width: double.infinity,
                    child: CustomPaint(
                      painter: _TrafficPainter(
                        slots,
                        maxBytes,
                        c.action,
                        c.direct,
                        c.border,
                        index,
                      ),
                    ),
                  ),
                ),
              ),
            );
          },
        ),
        const SizedBox(height: 8),
        Row(
          mainAxisAlignment: MainAxisAlignment.spaceBetween,
          children: [
            Text(
              fullTime(history.windowEnd - (history.hours - 1) * 3600000)
                  .substring(5, 16),
              style: Type.caption.copyWith(color: c.text3),
            ),
            Text(
              fullTime(history.windowEnd).substring(5, 16),
              style: Type.caption.copyWith(color: c.text3),
            ),
          ],
        ),
        const SizedBox(height: 8),
        Text(
          index == null
              ? '入站：公网 → 本机 · 出站：本机 → 公网'
              : '${fullTime(history.windowEnd - (history.hours - 1 - index) * 3600000).substring(0, 16)} · ${bucket == null ? '无采样' : '入站 ${formatBytes(bucket.bytesIn)} / 出站 ${formatBytes(bucket.bytesOut)} · 新建连接 ${bucket.connectionsTotal}${bucket.gap ? ' · 观测不完整' : ''}'}',
          key: const Key('traffic-detail'),
          style: Type.caption.copyWith(color: c.text2),
        ),
      ],
    );
  }
}

class _TrafficPainter extends CustomPainter {
  _TrafficPainter(
    this.slots,
    this.maximum,
    this.inColor,
    this.outColor,
    this.gridColor,
    this.selected,
  );
  final List<TrafficBucket?> slots;
  final int maximum;
  final Color inColor, outColor, gridColor;
  final int? selected;
  @override
  void paint(Canvas canvas, Size size) {
    final grid = Paint()..color = gridColor;
    for (var i = 0; i <= 3; i++) {
      final y = size.height * i / 3;
      canvas.drawLine(Offset(0, y), Offset(size.width, y), grid);
    }
    final step = size.width / slots.length;
    for (var i = 0; i < slots.length; i++) {
      final bucket = slots[i];
      if (i == selected) {
        canvas.drawRect(
          Rect.fromLTWH(i * step, 0, step, size.height),
          Paint()..color = gridColor.withValues(alpha: .35),
        );
      }
      if (bucket == null) continue;
      final width = math.max(.5, step * .32);
      void bar(int bytes, double offset, Color color) {
        if (bytes == 0) {
          canvas.drawLine(
            Offset(i * step + offset, size.height),
            Offset(i * step + offset + width, size.height),
            Paint()
              ..color = color
              ..strokeWidth = 2,
          );
          return;
        }
        final h = (bytes / maximum * size.height).clamp(0.0, size.height);
        canvas.drawRect(
          Rect.fromLTWH(i * step + offset, size.height - h, width, h),
          Paint()..color = color,
        );
      }

      bar(bucket.bytesIn, step * .12, inColor);
      bar(bucket.bytesOut, step * .54, outColor);
      if (bucket.gap) {
        for (double y = 0; y < size.height; y += 8) {
          canvas.drawLine(
            Offset((i + .5) * step, y),
            Offset((i + .5) * step, math.min(y + 3, size.height)),
            grid,
          );
        }
      }
    }
  }

  @override
  bool shouldRepaint(covariant _TrafficPainter old) =>
      old.slots != slots ||
      old.maximum != maximum ||
      old.selected != selected ||
      old.inColor != inColor ||
      old.outColor != outColor ||
      old.gridColor != gridColor;
}
