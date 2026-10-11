import 'dart:convert';
import 'dart:io';
import 'dart:ui' show PointerDeviceKind;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/models.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:rustfrp_app/shared/format.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

void main() {
  test('shared traffic fixture, missing fields and gaps', () {
    final fixture = jsonDecode(
      File('../rustfrp-protocol/fixtures/traffic-history.json')
          .readAsStringSync(),
    );
    final history = TrafficHistory.fromJson(fixture);
    expect((history.hours, history.bytesIn, history.bytesOut), (24, 24, 48));
    expect(history.slots.last?.connectionsTotal, 9);
    expect(history.slots.last?.gap, isTrue);
    expect(history.slots.take(23), everyElement(isNull));
    expect(
      () => history.items.add(const TrafficBucket(timestamp: 0)),
      throwsUnsupportedError,
    );
    expect(TrafficHistory.fromJson(null).items, isEmpty);
    final defaults = TrafficBucket.fromJson(null);
    expect(
      (defaults.timestamp, defaults.bytesIn, defaults.bytesOut, defaults.gap),
      (0, 0, 0, true),
    );
    final week = TrafficHistory.fromJson({
      'range_hours': 168,
      'window_end': 0,
      'items': [
        {'timestamp': 0, 'bytes_in': 0, 'gap': false},
      ],
    });
    expect(week.slots.length, 168);
    expect(week.slots.last?.gap, isFalse);
    expect(TrafficHistory.fromJson({'range_hours': 1}).hours, 24);
    final overview = Overview.fromJson({
      'traffic': {'bytes_in': 10, 'bytes_out': 20, 'connections': 3},
    });
    expect(
      (overview.bytesIn, overview.bytesOut, overview.connections),
      (10, 20, 3),
    );
    expect(formatBytes(-1), '0 B');
    expect(formatDuration(-5), '0 秒');
    expect(formatDuration(59), '59 秒');
    expect(formatDuration(120), '2 分钟');
    expect(formatDuration(3600), '1 小时');
    expect(formatDuration(3660), '1 小时 1 分钟');
    expect(formatDuration(86400), '1 天');
    expect(formatDuration(90000), '1 天 1 小时');
    expect(formatBytes(1024), '1.0 KiB');
    expect(formatBytes(1024 * 1024), '1.0 MiB');
    expect(formatBytes(1 << 50), contains('TiB'));
  });

  testWidgets('ranges, scope filters, details and missing observations', (
    tester,
  ) async {
    final server = FakeServer()..trafficWindowEnd = 23 * 3600000;
    final client = server.addClient('chart-device');
    final tunnel = server.addTunnel(client['id']! as String, 'chart-tunnel');
    server.traffic.addAll([
      {
        'timestamp': 0,
        'bytes_in': 1024,
        'bytes_out': 2048,
        'connections_total': 2,
        'gap': false,
      },
      {
        'timestamp': 3600000,
        'bytes_in': 0,
        'bytes_out': 0,
        'connections_total': 0,
        'gap': true,
      },
      {
        'timestamp': 23 * 3600000,
        'bytes_in': 4096,
        'bytes_out': 8192,
        'connections_total': 3,
        'gap': true,
      },
    ]);
    final h = await Harness.create(
      tester,
      server,
      size: const Size(1200, 1000),
    );
    expect(find.text('流量历史'), findsOneWidget);
    final chart = find.byKey(const Key('traffic-chart'));
    await tester.ensureVisible(chart);
    await settle(tester);
    final rect = tester.getRect(chart);
    await tester.tapAt(Offset(rect.left + rect.width * .98, rect.center.dy));
    await settle(tester);
    expect(find.textContaining('新建连接 3'), findsOneWidget);
    expect(find.textContaining('观测不完整').last, findsOneWidget);
    final custom = tester
        .widgetList<CustomPaint>(
          find.descendant(of: chart, matching: find.byType(CustomPaint)),
        )
        .first;
    expect(custom.painter!.shouldRepaint(custom.painter!), isFalse);
    await tester.tapAt(Offset(rect.left + rect.width * .5, rect.center.dy));
    await settle(tester);
    expect(find.textContaining('· 无采样'), findsOneWidget);
    await tester.tapAt(Offset(rect.left + rect.width * .015, rect.center.dy));
    await settle(tester);
    expect(find.textContaining('新建连接 2'), findsOneWidget);
    final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
    await mouse.addPointer(
      location: Offset(rect.left + rect.width * .06, rect.center.dy),
    );
    await mouse.moveTo(Offset(rect.left + rect.width * .065, rect.center.dy));
    await settle(tester);
    expect(find.textContaining('新建连接 0'), findsOneWidget);
    await mouse.removePointer();
    await tester.ensureVisible(find.byKey(const Key('traffic-range-168')));
    await tapKey(tester, 'traffic-range-168');
    expect(
      server.requests
          .lastWhere((r) => r.url.path.endsWith('/traffic'))
          .url
          .queryParameters['hours'],
      '168',
    );
    await tapKey(tester, 'traffic-client');
    await tapText(tester, 'chart-device');
    expect(server.requests.last.url.queryParameters['client_id'], client['id']);
    await tapKey(tester, 'traffic-tunnel');
    await tapText(tester, 'chart-tunnel');
    expect(server.requests.last.url.queryParameters['tunnel_id'], tunnel['id']);
    server.clients.clear();
    server.tunnels.clear();
    h.container.invalidate(clientsProvider);
    h.container.invalidate(tunnelsProvider);
    await settle(tester);
    expect(find.text('全部客户端'), findsOneWidget);
    expect(find.text('全部隧道'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('error retry, empty data and narrow dark layout', (tester) async {
    final server = FakeServer()..failTraffic = true;
    await Harness.create(
      tester,
      server,
      size: const Size(420, 1000),
      prefs: {'theme_mode': 'dark'},
    );
    await tester.scrollUntilVisible(
      find.text('加载失败'),
      150,
      scrollable: find.byType(Scrollable).last,
    );
    expect(find.textContaining('traffic unavailable'), findsOneWidget);
    server.failTraffic = false;
    await tapText(tester, '重试');
    expect(find.text('暂无流量采样'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });
  testWidgets('failed live refresh visibly keeps the previous sample', (
    tester,
  ) async {
    final server = FakeServer();
    server.traffic.add({
      'timestamp': 3600000,
      'bytes_in': 1024,
      'bytes_out': 2048,
      'gap': false,
    });
    final h = await Harness.create(
      tester,
      server,
      size: const Size(1200, 1000),
    );
    expect(find.text('入站 1.0 KiB'), findsOneWidget);
    server.failTraffic = true;
    h.tick.add(1);
    await settle(tester, 12);
    expect(find.byKey(const Key('traffic-stale')), findsOneWidget);
    expect(find.text('入站 1.0 KiB'), findsOneWidget);
    server.failTraffic = false;
    await tester.ensureVisible(find.text('重试'));
    await tapText(tester, '重试');
    expect(find.byKey(const Key('traffic-stale')), findsNothing);
    expect(find.text('出站 2.0 KiB'), findsOneWidget);
  });
}
