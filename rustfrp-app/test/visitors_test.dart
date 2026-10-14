import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/models.dart';
import 'package:rustfrp_app/shared/widgets.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

Future<void> fillReason(WidgetTester tester, String reason) async {
  await tester.enterText(
    find.widgetWithText(TextField, '操作理由（必填，会写入审计）').first,
    reason,
  );
  await settle(tester);
}

LButton save(WidgetTester tester) =>
    tester.widget<LButton>(find.byKey(const Key('visitor-save')));

Future<void> openClient(WidgetTester tester, String name) async {
  await tester.tap(find.byTooltip('客户端'));
  await settle(tester);
  await tester.tap(find.text(name));
  await settle(tester);
}

void main() {
  test('Visitor parses, compares and rejects payloads without an id', () {
    final v = Visitor.fromJson({
      'id': 'v1',
      'client_id': 'c',
      'name': 'ssh',
      'tunnel_id': 't',
      'bind_addr': '0.0.0.0',
      'bind_port': 2222,
      'enabled': false,
    });
    expect((v.bindAddr, v.bindPort, v.enabled), ('0.0.0.0', 2222, false));
    final transformed = Visitor.fromJson({
      'id': 'v-secure',
      'use_encryption': true,
      'use_compression': true,
    });
    expect(transformed.useEncryption && transformed.useCompression, isTrue);
    expect(v == Visitor.fromJson({'id': 'v1'}), isFalse);
    expect(
      Visitor.fromJson({'id': 'v1'}) == Visitor.fromJson({'id': 'v1'}),
      isTrue,
    );
    expect(
      Visitor.fromJson({'id': 'v1'}).hashCode,
      Visitor.fromJson({'id': 'v1'}).hashCode,
    );
    expect(() => Visitor.fromJson({'name': 'x'}), throwsA(anything));
    expect(Tunnel.fromJson({'id': 't', 'kind': 'stcp'}).isSecret, isTrue);
    expect(Tunnel.fromJson({'id': 't', 'kind': 'tcp'}).isSecret, isFalse);
  });

  testWidgets('client page: create, toggle, edit and delete visitors', (
    tester,
  ) async {
    final s = FakeServer();
    final consumer = s.addClient('laptop', isOnline: true);
    final provider = s.addClient('nas', isOnline: true);
    final stcp = s.addTunnel(provider['id']! as String, 'ssh', remote: 0);
    stcp['kind'] = 'stcp';
    final own = s.addTunnel(consumer['id']! as String, 'own', remote: 0);
    own['kind'] = 'stcp';
    final plain = s.addTunnel(provider['id']! as String, 'web', remote: 6001);
    plain['kind'] = 'tcp';
    await Harness.create(tester, s, size: const Size(1200, 1400));
    await openClient(tester, 'laptop');
    expect(find.text('访问者（0）'), findsOneWidget);
    expect(find.textContaining('访问者让这台设备'), findsOneWidget);

    await tapKey(tester, 'client-new-visitor');
    expect(find.byKey(const Key('visitor-name')), findsOneWidget);
    expect(save(tester).onPressed, isNull, reason: 'name and target required');
    await tester.enterText(find.byKey(const Key('visitor-name')), 'nas-ssh');
    await tester.tap(find.byKey(const Key('visitor-tunnel')));
    await settle(tester);
    // 只能选其他设备上的私密隧道：本机自己的 stcp 与普通 tcp 不在列表里。
    expect(find.text('nas / ssh（STCP）'), findsWidgets);
    expect(find.textContaining('laptop / own'), findsNothing);
    expect(find.textContaining('nas / web'), findsNothing);
    await tester.tap(find.text('nas / ssh（STCP）').last);
    await settle(tester);
    await tester.enterText(find.byKey(const Key('visitor-port')), 'abc');
    await settle(tester);
    expect(save(tester).onPressed, isNull, reason: 'port must be numeric');
    await tester.enterText(find.byKey(const Key('visitor-port')), '2222');
    await settle(tester);
    await tester.tap(find.byKey(const Key('visitor-use-encryption')));
    await tester.tap(find.byKey(const Key('visitor-use-compression')));
    await settle(tester);
    await tapKey(tester, 'visitor-save');
    await settle(tester, 10);
    expect(s.visitors.length, 1);
    expect(s.visitors.single['bind_port'], 2222);
    expect(s.visitors.single['tunnel_id'], stcp['id']);
    expect(s.visitors.single['use_encryption'], true);
    expect(s.visitors.single['use_compression'], true);
    expect(find.text('访问者（1）'), findsOneWidget);
    expect(find.textContaining('127.0.0.1:2222'), findsOneWidget);

    // 停用 / 编辑 / 删除。
    await tester.tap(
      find.descendant(
        of: find.byKey(Key('visitor-${s.visitors.single['id']}')),
        matching: find.byType(LSwitch),
      ),
    );
    await settle(tester, 10);
    expect(s.visitors.single['enabled'], false);
    await tester.tap(find.byTooltip('编辑访问者'));
    await settle(tester);
    await tester.enterText(find.byKey(const Key('visitor-port')), '');
    await tester.enterText(find.byKey(const Key('visitor-addr')), '0.0.0.0');
    await settle(tester);
    await tapKey(tester, 'visitor-save');
    await settle(tester, 10);
    expect(s.visitors.single['bind_addr'], '0.0.0.0');
    expect(s.visitors.single['bind_port'], 0);
    expect(find.textContaining('0.0.0.0:自动'), findsOneWidget);
    await tester.tap(find.byTooltip('删除访问者'));
    await settle(tester);
    await fillReason(tester, '不再需要');
    await tester.tap(find.widgetWithText(LButton, '删除'));
    await settle(tester, 10);
    expect(s.visitors, isEmpty);
    expect(find.text('访问者（0）'), findsOneWidget);
  });

  testWidgets('visitor editor: deleted targets and missing private tunnels', (
    tester,
  ) async {
    final s = FakeServer();
    final c = s.addClient('laptop', isOnline: true);
    s.addVisitor(c['id']! as String, 'orphan', 'missing-tunnel');
    await Harness.create(tester, s, size: const Size(1200, 1400));
    await openClient(tester, 'laptop');
    expect(find.textContaining('目标隧道已删除'), findsOneWidget);
    await tester.tap(find.byTooltip('编辑访问者'));
    await settle(tester);
    // 目标已不存在：下拉里仍显示当前值，不会崩溃。
    expect(find.text('当前目标（不可用）'), findsOneWidget);
    await tester.tap(find.text('取消'));
    await settle(tester);
    // 没有任何私密隧道可选：给出提示（新建时保存不可用）。
    await tester.tap(find.byTooltip('删除访问者'));
    await settle(tester);
    await fillReason(tester, 'x');
    await tester.tap(find.widgetWithText(LButton, '删除'));
    await settle(tester, 10);
    await tapKey(tester, 'client-new-visitor');
    expect(find.textContaining('还没有可访问的私密隧道'), findsOneWidget);
    expect(save(tester).onPressed, isNull);
  });

  testWidgets('visitor editor: server rejections keep the dialog open', (
    tester,
  ) async {
    final s = FakeServer();
    final c = s.addClient('laptop', isOnline: true);
    final other = s.addClient('nas', isOnline: true);
    final t = s.addTunnel(other['id']! as String, 'db', remote: 0);
    t['kind'] = 'sudp';
    s.addVisitor(c['id']! as String, 'dup', t['id']! as String);
    await Harness.create(tester, s, size: const Size(1200, 1400));
    await openClient(tester, 'laptop');
    await tapKey(tester, 'client-new-visitor');
    await tester.enterText(find.byKey(const Key('visitor-name')), 'dup');
    await tester.tap(find.byKey(const Key('visitor-tunnel')));
    await settle(tester);
    await tester.tap(find.text('nas / db（SUDP）').last);
    await settle(tester);
    await tapKey(tester, 'visitor-save');
    await settle(tester, 10);
    expect(find.textContaining('already used'), findsOneWidget);
    expect(find.byKey(const Key('visitor-name')), findsOneWidget);
  });

  testWidgets(
    'tunnel editor: stcp / sudp have no public port, proxy protocol or groups',
    (tester) async {
      final s = FakeServer();
      s.addClient('nas', isOnline: true);
      await Harness.create(tester, s, size: const Size(1200, 1600));
      await tester.tap(find.byTooltip('隧道管理'));
      await settle(tester);
      await tapKey(tester, 'new-tunnel');
      await tester.tap(find.text('选择客户端'));
      await settle(tester);
      await tester.tap(find.text('nas').last);
      await settle(tester);
      await tester.tap(find.text('TCP'));
      await settle(tester);
      await tester.tap(find.textContaining('STCP').last);
      await settle(tester);
      expect(find.byKey(const Key('tunnel-remote-port')), findsNothing);
      expect(find.byKey(const Key('tunnel-proxy-protocol')), findsNothing);
      expect(find.byKey(const Key('tunnel-lb-group')), findsNothing);
      expect(find.byKey(const Key('tunnel-bandwidth')), findsOneWidget);
      expect(find.textContaining('私密隧道没有公网端口'), findsOneWidget);
      await tester.enterText(find.byKey(const Key('tunnel-name')), 'ssh');
      await tester.enterText(find.byKey(const Key('tunnel-local-port')), '22');
      await settle(tester);
      await tapKey(tester, 'tunnel-save');
      await settle(tester, 10);
      expect(s.tunnels.single['kind'], 'stcp');
      expect(s.tunnels.single['remote_port'], 0);
      expect(find.textContaining('私密（经访问者）'), findsOneWidget);

      // SUDP 连限速也没有（数据报不限速）。
      await tapKey(tester, 'new-tunnel');
      await tester.tap(find.text('选择客户端'));
      await settle(tester);
      await tester.tap(find.text('nas').last);
      await settle(tester);
      await tester.tap(find.text('TCP'));
      await settle(tester);
      await tester.tap(find.text('SUDP（私密 UDP，需访问者）').last);
      await settle(tester);
      expect(find.byKey(const Key('tunnel-bandwidth')), findsNothing);
      expect(find.byKey(const Key('tunnel-max-connections')), findsOneWidget);
    },
  );
}
