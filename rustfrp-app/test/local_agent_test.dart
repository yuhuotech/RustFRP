import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/local_agent.dart';
import 'package:rustfrp_app/core/platform/local_backend_stub.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:rustfrp_app/features/local/local_page.dart';
import 'package:rustfrp_app/app/app.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

const cfg = LocalAgentConfig(
  binary: '/agent',
  controller: 'example.com:7000',
  name: '本机',
  stateDir: '/state',
);
const online = LocalAgentStatus(
  pid: 123,
  connected: true,
  revision: 7,
  proxies: 2,
  bytesIn: 1024,
  bytesOut: 2048,
  connections: 1,
);

class FakeLocal implements LocalAgentBackend {
  @override
  bool available = true;
  @override
  LocalAgentConfig defaults = cfg;
  LocalAgentStatus? current;
  Exception? error;
  Completer<void>? pending;
  int starts = 0, stops = 0, reads = 0;
  String? receivedToken;
  bool exits = false;
  List<String> logLines = ['agent connected'];
  @override
  Future<LocalAgentStatus?> status(LocalAgentConfig config) async {
    reads++;
    if (error != null) throw error!;
    return current;
  }

  @override
  Future<String> exportDiagnostics(LocalAgentConfig config) async {
    if (error != null) throw error!;
    return '/state/diagnostics-test.json';
  }

  List<Map<String, dynamic>> tunnelItems = [];
  @override
  Future<List<Map<String, dynamic>>> tunnels(LocalAgentConfig config) async {
    if (error != null) throw error!;
    return tunnelItems;
  }

  @override
  Future<List<String>> logs(LocalAgentConfig config) async {
    if (error != null) throw error!;
    return logLines;
  }

  @override
  Future<void> start(LocalAgentConfig config, String enrollToken) async {
    starts++;
    receivedToken = enrollToken;
    if (pending != null) await pending!.future;
    if (error != null) throw error!;
    if (!exits) current = online;
  }

  @override
  Future<void> stop(LocalAgentConfig config) async {
    stops++;
    if (pending != null) await pending!.future;
    if (error != null) throw error!;
    current = null;
  }
}

Future<ProviderContainer> make(
  FakeLocal backend, {
  Map<String, Object> prefs = const {},
  bool web = false,
}) async {
  SharedPreferences.setMockInitialValues(prefs);
  final p = await SharedPreferences.getInstance();
  return ProviderContainer(
    retry: (_, _) => null,
    overrides: [
      prefsProvider.overrideWithValue(p),
      sameOriginProvider.overrideWithValue(web),
      localBackendProvider.overrideWithValue(backend),
      apiFactoryProvider.overrideWithValue((u) => FakeServer().api(u)),
    ],
  );
}

void main() {
  test('models, shared fixture, unsupported web backend', () async {
    expect(LocalAgentConfig.fromJson(cfg.toJson()), cfg);
    expect(LocalAgentConfig.fromJson(cfg.toJson()).hashCode, cfg.hashCode);
    expect(cfg == const LocalAgentConfig(), false);
    expect(cfg.discoveryPath, '/state/control.json');
    final fixture = jsonDecode(
      await File('../rustfrp-protocol/fixtures/local-status.json')
          .readAsString(),
    ) as Map<String, dynamic>;
    final s = LocalAgentStatus.fromJson(fixture);
    expect([s.pid, s.connected, s.revision, s.proxies], [123, true, 7, 2]);
    expect([s.bytesIn, s.bytesOut, s.connections], [0, 0, 0]);
    expect(
      LocalAgentStatus.fromJson({...fixture, 'traffic': null}).bytesIn,
      isNull,
    );
    for (final value in [
      1,
      <String, dynamic>{},
      {'bytes_in': -1, 'bytes_out': 0, 'connections': 0},
      {'bytes_in': 1, 'bytes_out': 'x', 'connections': 0},
      {'bytes_in': 1, 'bytes_out': 0, 'connections': -1},
    ]) {
      expect(
        () => LocalAgentStatus.fromJson({...fixture, 'traffic': value}),
        throwsFormatException,
      );
    }
    for (final key in ['version', 'pid', 'connected', 'revision', 'proxies']) {
      expect(
        () => LocalAgentStatus.fromJson({...fixture, key: null}),
        throwsFormatException,
      );
    }
    for (final key in ['pid', 'revision', 'proxies']) {
      expect(
        () => LocalAgentStatus.fromJson({...fixture, key: -1}),
        throwsFormatException,
      );
    }
    const backend = UnsupportedLocalBackend();
    await expectLater(backend.exportDiagnostics(cfg), throwsUnsupportedError);
    await expectLater(backend.tunnels(cfg), throwsUnsupportedError);
    await expectLater(backend.logs(cfg), throwsUnsupportedError);
    expect(backend.available, false);
    expect(backend.defaults, const LocalAgentConfig());
    expect(await backend.status(cfg), isNull);
    await expectLater(backend.start(cfg, 't'), throwsUnsupportedError);
    await expectLater(backend.stop(cfg), throwsUnsupportedError);
    expect(createLocalBackend(), isA<UnsupportedLocalBackend>());
  });
  test('mode hidden on web; token never persisted', () async {
    final web = await make(FakeLocal(), web: true, prefs: {'local_mode': true});
    expect(web.read(localModeProvider), false);
    await web.read(localModeProvider.notifier).select(true);
    expect(web.read(localModeProvider), false);
    web.dispose();
    final b = FakeLocal();
    final c = await make(b);
    addTearDown(c.dispose);
    expect(c.read(localModeProvider), false);
    await c.read(localModeProvider.notifier).select(true);
    expect(c.read(localModeProvider), true);
    await c.read(localAgentProvider.notifier).start(cfg, 'secret-test-only');
    expect(c.read(localAgentProvider).status?.connected, true);
    final prefs = c.read(prefsProvider);
    expect(prefs.getString('local_agent'), jsonEncode(cfg.toJson()));
    expect(
      prefs.getKeys().any(
        (k) => prefs.get(k).toString().contains('secret-test-only'),
      ),
      false,
    );
    await c.read(localAgentProvider.notifier).stop();
    expect(c.read(localAgentProvider).status, isNull);
    await c.read(localModeProvider.notifier).select(false);
    expect(c.read(localModeProvider), false);
  });
  test('immediate locking; closing UI preserves detached agent', () async {
    final b = FakeLocal()..pending = Completer<void>();
    final c = await make(b);
    final n = c.read(localAgentProvider.notifier);
    final a = n.start(cfg, 't');
    expect(c.read(localAgentProvider).busy, true);
    await n.start(cfg, 'again');
    await n.refresh();
    await n.stop();
    b.pending!.complete();
    await a;
    expect(b.starts, 1);
    expect(b.stops, 0);
    b.pending = Completer<void>();
    final stop = n.stop();
    await n.stop();
    expect(b.stops, 1);
    b.pending!.complete();
    await stop;
    b.pending = null;
    await n.start(cfg, '');
    c.dispose();
    expect(b.current, online);
  });
  test('failures retain last process; corrupt preferences; disposed async completion', () async {
    final b = FakeLocal()..current = online;
    final c = await make(b, prefs: {'local_agent': jsonEncode(cfg.toJson())});
    final n = c.read(localAgentProvider.notifier);
    await n.refresh();
    await n.start(const LocalAgentConfig(controller: 'other'), 't');
    expect(c.read(localAgentProvider).error, contains('先停止'));
    expect(b.starts, 0);
    b.error = const FormatException('认证失败');
    await n.refresh();
    expect(c.read(localAgentProvider).status, online);
    expect(c.read(localAgentProvider).error, '认证失败');
    await n.stop();
    expect(c.read(localAgentProvider).status, online);
    b.error = Exception('secret');
    await n.start(cfg, 't');
    expect(c.read(localAgentProvider).error, isNot(contains('secret')));
    c.dispose();
    final corrupt = await make(FakeLocal(), prefs: {'local_agent': 'broken'});
    expect(corrupt.read(localAgentProvider).error, contains('损坏'));
    corrupt.dispose();
    final exited = await make(FakeLocal()..exits = true);
    await exited.read(localAgentProvider.notifier).start(cfg, 't');
    expect(exited.read(localAgentProvider).error, contains('已退出'));
    exited.dispose();
    for (final error in [false, true]) {
      final delayed = FakeLocal()..pending = Completer<void>();
      final late = await make(delayed);
      final future = late.read(localAgentProvider.notifier).start(cfg, 't');
      await Future<void>.delayed(Duration.zero);
      late.dispose();
      if (error) delayed.error = Exception('failed');
      delayed.pending!.complete();
      await future;
    }
  });
  test('empty profile skips polling', () async {
    final b = FakeLocal()..defaults = const LocalAgentConfig();
    final c = await make(b);
    addTearDown(c.dispose);
    await c.read(localAgentProvider.notifier).refresh();
    expect(b.reads, 0);
  });
  testWidgets('desktop local start/stop/reconnect/errors and mode switching', (
    tester,
  ) async {
    final b = FakeLocal();
    final c = await make(
      b,
      prefs: {'local_mode': true, 'local_agent': jsonEncode(cfg.toJson())},
    );
    addTearDown(c.dispose);
    tester.view.physicalSize = const Size(1200, 1100);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      UncontrolledProviderScope(container: c, child: const RustFrpApp()),
    );
    await settle(tester);
    expect(find.byType(LocalPage), findsOneWidget);
    expect(find.text('未运行'), findsOneWidget);
    await tester.enterText(find.byKey(const Key('local-token')), 'test-token');
    await tester.ensureVisible(find.byKey(const Key('local-start')));
    await tapKey(tester, 'local-start');
    await tester.tap(find.byTooltip('返回首页'));
    await settle(tester);
    expect(
      tester.getTopLeft(find.byKey(const Key('local-status'))).dy,
      lessThan(120),
    );
    expect(find.text('已连接'), findsOneWidget);
    b.tunnelItems = [
      {
        'id': 'tunnel-id',
        'kind': 'tcp',
        'local_ip': '::1',
        'local_port': 22,
        'remote_port': 0,
      },
      {
        'id': 'web-id',
        'kind': 'http',
        'local_ip': 'localhost',
        'local_port': 8080,
        'remote_port': 9000,
      },
    ];
    await tester.ensureVisible(find.byKey(const Key('local-tunnels')));
    await tapKey(tester, 'local-tunnels');
    expect(find.text('本地目标 [::1]:22'), findsOneWidget);
    expect(find.text('公网地址尚未确认'), findsOneWidget);
    expect(find.text('配置的远程端口 9000'), findsOneWidget);
    await tapKey(tester, 'local-tunnels-refresh');
    await tapKey(tester, 'local-tunnels-close');
    b.tunnelItems = [];
    await tapKey(tester, 'local-tunnels');
    expect(find.text('当前没有已应用隧道'), findsOneWidget);
    await tapKey(tester, 'local-tunnels-close');
    b.error = const FormatException('隧道读取失败');
    await tapKey(tester, 'local-tunnels');
    expect(find.text('FormatException: 隧道读取失败'), findsOneWidget);
    b.error = null;
    await tapText(tester, '重试');
    expect(find.text('当前没有已应用隧道'), findsOneWidget);
    await tapKey(tester, 'local-tunnels-close');
    await tester.ensureVisible(find.byKey(const Key('local-logs')));
    await tapKey(tester, 'local-logs');
    expect(find.text('agent connected'), findsOneWidget);
    await tapKey(tester, 'local-logs-refresh');
    expect(find.text('agent connected'), findsOneWidget);
    await tapKey(tester, 'local-logs-close');
    await tester.ensureVisible(find.byKey(const Key('local-diagnostics')));
    await tapKey(tester, 'local-diagnostics');
    expect(find.textContaining('诊断文件已保存'), findsOneWidget);
    b.error = const FormatException('导出错误');
    await tapKey(tester, 'local-diagnostics');
    expect(find.textContaining('导出失败'), findsOneWidget);
    b.error = null;
    await tapKey(tester, 'local-logs');
    b.error = const FormatException('日志读取失败');
    await tapKey(tester, 'local-logs-close');
    await tapKey(tester, 'local-logs');
    expect(find.text('FormatException: 日志读取失败'), findsOneWidget);
    b.error = null;
    b.logLines = [];
    await tapText(tester, '重试');
    expect(find.text('暂无日志'), findsOneWidget);
    await tapKey(tester, 'local-logs-close');
    expect(find.text('1.0 KiB'), findsOneWidget);
    expect(find.text('2.0 KiB'), findsOneWidget);
    await tester.ensureVisible(find.text('本次运行累计，重启后归零'));
    await settle(tester);
    expect(find.text('本次运行累计，重启后归零'), findsOneWidget);
    expect(b.receivedToken, 'test-token');
    expect(find.text('2'), findsOneWidget);
    b.current = const LocalAgentStatus(
      pid: 123,
      connected: false,
      revision: 7,
      proxies: 0,
    );
    await tapKey(tester, 'local-refresh');
    expect(find.text('连接中 / 重连中'), findsOneWidget);
    b.error = const FormatException('测试错误');
    await tapKey(tester, 'local-refresh');
    expect(find.text('状态暂不可用'), findsOneWidget);
    b.error = null;
    await tapKey(tester, 'local-stop');
    expect(find.text('未运行'), findsOneWidget);
    expect(
      tester
          .widget<TextField>(
            find.descendant(
              of: find.byKey(const Key('local-token')),
              matching: find.byType(TextField),
            ),
          )
          .controller!
          .text,
      '',
    );
    b.error = const FormatException('令牌错误');
    await tapKey(tester, 'local-start');
    expect(find.text('令牌错误'), findsOneWidget);
    await tapKey(tester, 'admin-mode');
    expect(find.byKey(const Key('login-submit')), findsOneWidget);
    await tapKey(tester, 'local-mode');
    expect(find.byType(LocalPage), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });
  testWidgets('narrow dark restore and periodic refresh', (tester) async {
    final b = FakeLocal()..current = online;
    final c = await make(b, prefs: {'local_mode': true, 'theme_mode': 'dark'});
    addTearDown(c.dispose);
    tester.view.physicalSize = const Size(400, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      UncontrolledProviderScope(container: c, child: const RustFrpApp()),
    );
    await settle(tester);
    expect(find.text('已连接'), findsOneWidget);
    b.current = null;
    await tester.pump(const Duration(seconds: 2));
    await settle(tester);
    expect(find.byKey(const Key('local-controller')), findsOneWidget);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });
  test('diagnostic format bounds and redacts endpoint and secret logs', () {
    final data = jsonDecode(
      diagnosticJson(online, [
        'ok',
        'PASSWORD=test',
        'http://host',
        'C:\temp',
      ], DateTime.utc(2026)),
    ) as Map<String, dynamic>;
    expect(data['traffic']['bytes_in'], 1024);
    expect(data['logs'], ['ok', '[redacted]', '[redacted]', '[redacted]']);
    expect(data['captured_at'], '2026-01-01T00:00:00.000Z');
    expect(
      () => diagnosticJson(online, List.filled(129, 'x'), DateTime.now()),
      throwsFormatException,
    );
    expect(
      () => diagnosticJson(online, ['中' * 200], DateTime.now()),
      throwsFormatException,
    );
    const old = LocalAgentStatus(
      pid: 1,
      connected: false,
      revision: 0,
      proxies: 0,
    );
    expect(
      jsonDecode(diagnosticJson(old, [], DateTime.now()))['traffic'],
      isNull,
    );
  });
}
