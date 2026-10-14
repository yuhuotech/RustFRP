import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/models.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:rustfrp_app/shared/widgets.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

Future<void> openNodes(WidgetTester tester) async {
  await tester.tap(find.byTooltip('节点管理'));
  await settle(tester);
}

Future<void> fillReason(WidgetTester tester, String reason) async {
  await tester.enterText(
    find.widgetWithText(TextField, '操作理由（必填，会写入审计）').first,
    reason,
  );
  await settle(tester);
}

void main() {
  testWidgets(
    'with the built-in node disabled, enrollment targets an external node',
    (tester) async {
      final s = FakeServer()..builtinEnabled = false;
      final n = s.addNode('hk');
      await Harness.create(tester, s);
      await tester.tap(find.byTooltip('客户端'));
      await settle(tester);
      await tester.tap(find.text('接入新客户端').first);
      await settle(tester);
      await tester.enterText(find.byKey(const Key('enroll-name')), 'laptop');
      await settle(tester);
      await tapKey(tester, 'enroll-create');
      await settle(tester, 10);
      final req = s.requests.lastWhere(
        (r) => r.url.path.endsWith('/enroll-tokens') && r.method == 'POST',
      );
      expect(req.body, contains('"node_id":"${n['id']}"'));
    },
  );

  testWidgets('node list shows builtin and external nodes with their state', (
    tester,
  ) async {
    final s = FakeServer();
    final ok = s.addNode('hk', clients: 2);
    s.addNode('gone', isOnline: false, host: '');
    s.addNode('off', disabled: true);
    await Harness.create(tester, s);
    await openNodes(tester);
    expect(find.text('节点管理'), findsWidgets);
    expect(find.textContaining('服务端节点承载客户端连接和隧道流量'), findsOneWidget);
    expect(find.text('服务端节点'), findsOneWidget);
    expect(find.text('公网接入地址'), findsOneWidget);
    expect(find.text('服务能力'), findsOneWidget);
    expect(find.text('内置节点'), findsOneWidget);
    expect(find.text('hk'), findsOneWidget);
    expect(find.text('2 台'), findsOneWidget);
    final row = tester.getRect(find.byKey(Key('node-row-${ok['id']}')));
    expect(row.width, greaterThan(800));
    expect(row.height, inInclusiveRange(56, 100));
    expect(find.textContaining('未配置公网地址'), findsWidgets);

    await tester.tap(find.byKey(Key('node-row-${ok['id']}')));
    await settle(tester);
    expect(find.text('运行状态'), findsOneWidget);
    expect(find.text('网络与接入'), findsOneWidget);
    expect(find.text('服务能力'), findsOneWidget);
    expect(find.text('身份与安全'), findsOneWidget);
    expect(find.text('管理操作'), findsOneWidget);
    expect(find.text('0.2.0'), findsOneWidget);
    expect(find.text('20000-20100'), findsOneWidget);
    expect(find.text('n.example.com'), findsOneWidget);
    expect(find.textContaining('独立服务端节点是单独运行的 Edge'), findsOneWidget);
    expect(find.byTooltip('重命名'), findsOneWidget);
    await tapKey(tester, 'node-back');
    expect(find.byKey(const Key('new-node')), findsOneWidget);
  });

  testWidgets('rename, disable and delete an external node', (tester) async {
    final s = FakeServer();
    final n = s.addNode('hk');
    await Harness.create(tester, s);
    await openNodes(tester);
    await tester.tap(find.byKey(Key('node-row-${n['id']}')));
    await settle(tester);

    await tester.tap(find.byTooltip('重命名'));
    await settle(tester);
    await tester.enterText(find.byKey(const Key('node-rename-field')), 'tokyo');
    await tester.tap(find.widgetWithText(LButton, '保存'));
    await settle(tester, 10);
    expect(s.extraNodes.first['name'], 'tokyo');
    expect(find.text('tokyo'), findsWidgets);

    await tester.tap(find.byTooltip('停用'));
    await settle(tester);
    await fillReason(tester, '维护');
    await tester.tap(find.widgetWithText(LButton, '停用').last);
    await settle(tester, 10);
    expect(s.extraNodes.first['disabled_at'], isNotNull);
    expect(find.text('已停用'), findsWidgets);

    await tester.tap(find.byTooltip('启用'));
    await settle(tester);
    await fillReason(tester, '恢复');
    await tester.tap(find.widgetWithText(LButton, '启用').last);
    await settle(tester, 10);
    expect(s.extraNodes.first['disabled_at'], isNull);

    // 仍有客户端：服务器拒绝，面板留在详情页并提示。
    s.extraNodes.first['clients'] = 1;
    await tester.tap(find.byTooltip('删除服务端节点'));
    await settle(tester);
    await fillReason(tester, '下线');
    await tester.tap(find.widgetWithText(LButton, '删除').last);
    await settle(tester, 10);
    expect(s.extraNodes, hasLength(1));
    expect(find.textContaining('node still serves clients'), findsOneWidget);
    s.extraNodes.first['clients'] = 0;
    await tester.tap(find.byTooltip('删除服务端节点'));
    await settle(tester);
    await fillReason(tester, '下线');
    await tester.tap(find.widgetWithText(LButton, '删除').last);
    await settle(tester, 10);
    expect(s.extraNodes, isEmpty);
    expect(
      find.byKey(const Key('new-node')),
      findsOneWidget,
      reason: 'back on the list',
    );
  });

  testWidgets('enrolling a node shows the edge command and waits for it', (
    tester,
  ) async {
    final s = FakeServer();
    final h = await Harness.create(tester, s);
    await openNodes(tester);
    await tapKey(tester, 'new-node');
    expect(find.text('接入服务端节点'), findsWidgets);
    await tester.enterText(find.byKey(const Key('enroll-name')), '香港节点');
    await settle(tester);
    await tapKey(tester, 'enroll-create');
    await settle(tester, 10);
    final req = s.requests.lastWhere(
      (r) => r.url.path.endsWith('/enroll-tokens') && r.method == 'POST',
    );
    expect(req.body, contains('"kind":"node"'));
    expect(find.textContaining('--mode edge'), findsOneWidget);
    expect(find.textContaining('<this-node-public-host>'), findsWidgets);
    expect(find.text('等待节点上线…'), findsOneWidget);

    // 节点用掉令牌并上线：对话框显示成功，可跳转到节点详情。
    final node = s.addNode('香港节点');
    s.tokens.last['used_at'] = 1700000300000;
    s.tokens.last['client_id'] = node['id'];
    h.container.invalidate(nodesProvider);
    h.container.invalidate(tokensProvider);
    await settle(tester, 10);
    expect(find.text('接入成功'), findsOneWidget);
    expect(find.byKey(const Key('enroll-joined-node')), findsOneWidget);
    await tester.tap(find.widgetWithText(LButton, '查看服务端节点'));
    await settle(tester);
    expect(
      find.text('0.2.0'),
      findsOneWidget,
      reason: 'the node detail is open',
    );
  });

  testWidgets('client enrollment can target an external node', (tester) async {
    final s = FakeServer();
    final n = s.addNode('hk');
    await Harness.create(tester, s);
    await tester.tap(find.byTooltip('客户端'));
    await settle(tester);
    await tester.tap(find.text('接入新客户端').first);
    await settle(tester);
    expect(find.byKey(const Key('enroll-node')), findsOneWidget);
    await tester.enterText(find.byKey(const Key('enroll-name')), 'laptop');
    await tester.tap(find.byKey(const Key('enroll-node')));
    await settle(tester);
    await tester.tap(find.text('hk').last);
    await settle(tester);
    await tapKey(tester, 'enroll-create');
    await settle(tester, 10);
    final req = s.requests.lastWhere(
      (r) => r.url.path.endsWith('/enroll-tokens') && r.method == 'POST',
    );
    expect(req.body, contains('"node_id":"${n['id']}"'));
    expect(req.body, contains('"kind":"client"'));
  });

  testWidgets('tunnel addresses use the node the client is attached to', (
    tester,
  ) async {
    final s = FakeServer();
    final n = s.addNode('hk');
    final c = s.addClient('nas', isOnline: true);
    c['node_id'] = n['id'];
    s.addTunnel(c['id']! as String, 'ssh', remote: 20001, state: 'running');
    final d = s.addClient('local');
    s.addTunnel(d['id']! as String, 'db', remote: 6001, state: 'running');
    await Harness.create(tester, s);
    await tester.tap(find.byTooltip('隧道管理'));
    await settle(tester);
    expect(find.textContaining('edge.example.com:20001'), findsOneWidget);
    expect(find.textContaining('frp.example.com:6001'), findsOneWidget);
    expect(ClientDevice.fromJson({'id': 'x', 'node_id': 'n1'}).nodeId, 'n1');
  });
}
