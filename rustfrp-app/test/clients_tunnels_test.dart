import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/models.dart';
import 'package:rustfrp_app/features/tunnels/tunnels_view.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:rustfrp_app/shared/widgets.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

Future<void> openClients(WidgetTester tester) async {
  await tester.tap(find.byTooltip('客户端'));
  await settle(tester);
}

Future<void> fillReason(WidgetTester tester, String reason) async {
  await tester.enterText(find.widgetWithText(TextField, '操作理由（必填，会写入审计）').first, reason);
  await settle(tester);
}

void main() {
  testWidgets('clients list: rows show state, selecting shows details and tunnels', (tester) async {
    final s = FakeServer();
    final a = s.addClient('nas', isOnline: true);
    final b = s.addClient('old-laptop', os: 'macos', disabled: true);
    s.addTunnel(a['id']! as String, 'ssh', error: '');
    await Harness.create(tester, s);
    await openClients(tester);
    expect(find.text('nas'), findsOneWidget);
    expect(find.textContaining('已停用'), findsOneWidget);
    expect(find.textContaining('Linux · 在线'), findsOneWidget);

    await tester.tap(find.byKey(Key('client-row-${a['id']}')));
    await settle(tester);
    expect(find.text('设备信息'), findsOneWidget);
    expect(find.text('隧道（1）'), findsOneWidget);
    expect(find.byKey(const Key('client-new-tunnel')), findsOneWidget);
    expect(find.text('在线'), findsWidgets);

    await tester.tap(find.byKey(Key('client-row-${b['id']}')));
    await settle(tester);
    expect(find.text('这台客户端还没有隧道。'), findsOneWidget);
    expect(find.text('已停用'), findsWidgets);
    expect(find.byTooltip('强制下线'), findsNothing, reason: 'offline devices cannot be kicked');
  });

  testWidgets('clients: empty state offers enrollment to admins only', (tester) async {
    await Harness.create(tester, FakeServer());
    await openClients(tester);
    expect(find.text('还没有客户端'), findsOneWidget);
    expect(find.text('接入新客户端'), findsWidgets);

    final v = FakeServer(role: 'viewer');
    await Harness.create(tester, v);
    await openClients(tester);
    expect(find.text('还没有客户端'), findsOneWidget);
    expect(find.text('接入新客户端'), findsNothing);
  });

  testWidgets('rename a client', (tester) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    final h = await Harness.create(tester, s);
    h.container.read(selectedClientProvider.notifier).select(c['id']! as String);
    await openClients(tester);
    await tester.tap(find.byTooltip('重命名'));
    await settle(tester);
    await tester.enterText(find.byKey(const Key('rename-field')), 'office-nas');
    await tapText(tester, '保存');
    expect(s.clients.single['name'], 'office-nas');
    expect(find.text('office-nas'), findsWidgets);
    // Unchanged / cancelled renames send nothing.
    final before = s.requests.length;
    await tester.tap(find.byTooltip('重命名'));
    await settle(tester);
    await tapText(tester, '取消');
    expect(s.requests.length, before);
  });

  testWidgets('disable, enable, kick and delete require a reason and update the list', (tester) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    s.addTunnel(c['id']! as String, 'ssh');
    final h = await Harness.create(tester, s);
    h.container.read(selectedClientProvider.notifier).select(c['id']! as String);
    await openClients(tester);

    // Disable: confirm stays disabled until a reason is typed.
    await tester.tap(find.byTooltip('停用'));
    await settle(tester);
    expect(find.text('停用客户端'), findsOneWidget);
    final confirm = find.widgetWithText(LButton, '停用');
    expect(tester.widget<LButton>(confirm).onPressed, isNull);
    await fillReason(tester, '设备丢失');
    await tester.tap(confirm);
    await settle(tester, 10);
    expect(s.clients.single['disabled_at'], isNotNull);
    expect(s.requests.where((r) => r.method == 'PATCH').last.body, contains('设备丢失'));

    // Enable again.
    await tester.tap(find.byTooltip('启用'));
    await settle(tester);
    await fillReason(tester, '找回了');
    await tester.tap(find.widgetWithText(LButton, '启用'));
    await settle(tester, 10);
    expect(s.clients.single['disabled_at'], isNull);

    // Kick shows a confirmation toast.
    await tester.tap(find.byTooltip('强制下线'));
    await settle(tester);
    await fillReason(tester, '重置连接');
    await tester.tap(find.widgetWithText(LButton, '下线'));
    await settle(tester, 10);
    expect(find.text('已发送下线指令'), findsOneWidget);

    // Delete removes the client and its tunnels, and clears the selection.
    await tester.tap(find.byTooltip('删除客户端'));
    await settle(tester);
    await fillReason(tester, '退役');
    await tester.tap(find.widgetWithText(LButton, '删除'));
    await settle(tester, 10);
    expect(s.clients, isEmpty);
    expect(s.tunnels, isEmpty);
    expect(h.container.read(selectedClientProvider), isNull);
    expect(find.text('还没有客户端'), findsOneWidget);
  });

  testWidgets('a rejected write shows the server message as a toast', (tester) async {
    final s = FakeServer();
    final c = s.addClient('nas');
    final h = await Harness.create(tester, s);
    h.container.read(selectedClientProvider.notifier).select(c['id']! as String);
    await openClients(tester);
    await tester.tap(find.byTooltip('删除客户端'));
    await settle(tester);
    await fillReason(tester, '退役');
    s.failNext = true;
    await tester.tap(find.widgetWithText(LButton, '删除'));
    await settle(tester, 10);
    expect(find.text('internal error'), findsOneWidget);
    expect(s.clients, isNotEmpty);
  });

  testWidgets('selected client that no longer exists, and load errors', (tester) async {
    final s = FakeServer();
    final h = await Harness.create(tester, s);
    h.container.read(selectedClientProvider.notifier).select('ghost');
    await openClients(tester);
    expect(find.text('客户端不存在'), findsOneWidget);

    s.offline = true;
    h.tick.add(1);
    await settle(tester, 10);
    // Previous data stays visible when a refresh fails; the connection dot turns red instead.
    expect(find.byTooltip('无法连接控制器'), findsOneWidget);
  });

  testWidgets('tunnels page: list, filter, toggle, edit, delete', (tester) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    final id = c['id']! as String;
    final ssh = s.addTunnel(id, 'ssh', remote: 6000);
    s.addTunnel(id, 'web', local: 80, remote: 6001, state: 'error', error: 'listen on port 6001 failed');
    s.addTunnel(id, 'db', local: 5432, remote: 0, state: 'stopped', enabled: false);
    await Harness.create(tester, s);
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    expect(find.text('ssh'), findsOneWidget);
    expect(find.text('listen on port 6001 failed'), findsOneWidget);
    expect(find.textContaining('frp.example.com:6000'), findsOneWidget);
    expect(find.textContaining('自动分配'), findsOneWidget);
    expect(find.text('已停用'), findsOneWidget);

    // Filter column.
    await tapKey(tester, 'filter-error');
    expect(find.text('ssh'), findsNothing);
    expect(find.text('web'), findsOneWidget);
    await tapKey(tester, 'filter-pending');
    expect(find.text('没有符合条件的隧道'), findsOneWidget);
    await tapKey(tester, 'filter-all');
    expect(find.text('ssh'), findsOneWidget);

    // Toggle the first tunnel off.
    await tester.tap(find.byType(LSwitch).first);
    await settle(tester, 10);
    expect(ssh['enabled'], isFalse);

    // Edit.
    await tester.tap(find.byTooltip('编辑').first);
    await settle(tester);
    expect(find.text('编辑隧道'), findsOneWidget);
    await tester.enterText(find.byKey(const Key('tunnel-name')), 'ssh-renamed');
    await tester.enterText(find.byKey(const Key('tunnel-local-port')), '2222');
    await settle(tester);
    await tapKey(tester, 'tunnel-save');
    expect(ssh['name'], 'ssh-renamed');
    expect(ssh['local_port'], 2222);

    // Delete.
    await tester.tap(find.byTooltip('删除').first);
    await settle(tester);
    await fillReason(tester, '不再需要');
    await tester.tap(find.widgetWithText(LButton, '删除'));
    await settle(tester, 10);
    expect(s.tunnels.any((t) => t['name'] == 'ssh-renamed'), isFalse);
  });

  testWidgets('create a tunnel: validation, server conflicts, success', (tester) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    s.addTunnel(c['id']! as String, 'taken', remote: 6000);
    await Harness.create(tester, s);
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    await tapKey(tester, 'new-tunnel');
    expect(find.text('新建隧道'), findsWidgets);

    // Save is disabled until the form is valid.
    GestureDetector saveGesture() => tester.widget<GestureDetector>(find.descendant(of: find.byKey(const Key('tunnel-save')), matching: find.byType(GestureDetector)).first);
    expect(saveGesture().onTap, isNull);
    // Choose the client.
    await tester.tap(find.text('选择客户端'));
    await settle(tester);
    await tester.tap(find.text('nas').last);
    await settle(tester);
    await tester.enterText(find.byKey(const Key('tunnel-name')), 'ssh');
    await tester.enterText(find.byKey(const Key('tunnel-local-port')), '99999');
    await settle(tester);
    expect(saveGesture().onTap, isNull, reason: 'port out of range');
    await tester.enterText(find.byKey(const Key('tunnel-local-port')), '22');
    await tester.enterText(find.byKey(const Key('tunnel-remote-port')), 'abc');
    await settle(tester);
    expect(saveGesture().onTap, isNull, reason: 'remote port must be numeric');
    await tester.enterText(find.byKey(const Key('tunnel-remote-port')), '6000');
    await settle(tester);
    await tapKey(tester, 'tunnel-save');
    expect(find.textContaining('already used'), findsOneWidget, reason: 'the server rejects the conflicting port; the dialog stays open');
    expect(find.text('新建隧道'), findsWidgets);

    await tester.enterText(find.byKey(const Key('tunnel-remote-port')), '');
    await settle(tester);
    await tapKey(tester, 'tunnel-save');
    await settle(tester, 10);
    expect(s.tunnels.where((t) => t['name'] == 'ssh'), hasLength(1));
    expect(s.tunnels.last['remote_port'], 0);
  });

  testWidgets('new tunnel is disabled with no clients; a viewer cannot edit', (tester) async {
    await Harness.create(tester, FakeServer());
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    expect(find.text('先接入一台客户端，再为它创建隧道。'), findsOneWidget);
    final btn = tester.widget<LButton>(find.byKey(const Key('new-tunnel')));
    expect(btn.onPressed, isNull);

    final v = FakeServer(role: 'viewer');
    final c = v.addClient('nas');
    v.addTunnel(c['id']! as String, 'ssh');
    await Harness.create(tester, v);
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    expect(find.text('ssh'), findsOneWidget);
    expect(find.byKey(const Key('new-tunnel')), findsNothing);
    expect(find.byType(LSwitch), findsNothing);
    expect(find.byTooltip('编辑'), findsNothing);
  });

  testWidgets('tunnel editor from a client page preselects that client', (tester) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    final h = await Harness.create(tester, s);
    h.container.read(selectedClientProvider.notifier).select(c['id']! as String);
    await openClients(tester);
    await tapKey(tester, 'client-new-tunnel');
    await tester.enterText(find.byKey(const Key('tunnel-name')), 'rdp');
    await tester.enterText(find.byKey(const Key('tunnel-local-port')), '3389');
    await settle(tester);
    await tapKey(tester, 'tunnel-save');
    await settle(tester, 10);
    expect(s.tunnels.single['client_id'], c['id']);
    expect(find.text('rdp'), findsOneWidget);
  });

  testWidgets('create UDP and HTTP tunnels; domains are required for web types; addresses are shown as URLs', (tester) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    await Harness.create(tester, s);
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);

    // UDP
    await tapKey(tester, 'new-tunnel');
    await tester.tap(find.text('选择客户端'));
    await settle(tester);
    await tester.tap(find.text('nas').last);
    await settle(tester);
    await tester.tap(find.text('TCP'));
    await settle(tester);
    await tester.tap(find.text('UDP').last);
    await settle(tester);
    await tester.enterText(find.byKey(const Key('tunnel-name')), 'dns');
    await tester.enterText(find.byKey(const Key('tunnel-local-port')), '53');
    await tester.enterText(find.byKey(const Key('tunnel-remote-port')), '5353');
    await settle(tester);
    await tapKey(tester, 'tunnel-save');
    await settle(tester, 10);
    expect(s.tunnels.single['kind'], 'udp');
    expect(s.tunnels.single['remote_port'], 5353);
    expect(find.textContaining('frp.example.com:5353'), findsOneWidget);

    // HTTP: the domain fields replace the public port; save stays disabled without a domain or subdomain.
    await tapKey(tester, 'new-tunnel');
    await tester.tap(find.text('选择客户端'));
    await settle(tester);
    await tester.tap(find.text('nas').last);
    await settle(tester);
    await tester.tap(find.text('TCP'));
    await settle(tester);
    await tester.tap(find.textContaining('HTTP（按域名）').last);
    await settle(tester);
    expect(find.byKey(const Key('tunnel-remote-port')), findsNothing);
    expect(find.byKey(const Key('tunnel-rewrite')), findsOneWidget);
    await tester.enterText(find.byKey(const Key('tunnel-name')), 'site');
    await tester.enterText(find.byKey(const Key('tunnel-local-port')), '80');
    await settle(tester);
    GestureDetector save() => tester.widget<GestureDetector>(find.descendant(of: find.byKey(const Key('tunnel-save')), matching: find.byType(GestureDetector)).first);
    expect(save().onTap, isNull);
    await tester.enterText(find.byKey(const Key('tunnel-domains')), 'WWW.example.com, b.example.com');
    await tester.enterText(find.byKey(const Key('tunnel-rewrite')), 'internal.lan');
    await settle(tester);
    expect(save().onTap, isNotNull);
    await tapKey(tester, 'tunnel-save');
    await settle(tester, 10);
    final web = s.tunnels.last;
    expect(web['kind'], 'http');
    expect(web['custom_domains'], ['WWW.example.com', 'b.example.com']);
    expect(web['host_header_rewrite'], 'internal.lan');
    expect(find.textContaining('http://WWW.example.com:8080'), findsOneWidget);
    expect(find.textContaining('(+1)'), findsOneWidget);

    // Editing keeps the type fixed and updates domains.
    await tester.tap(find.byTooltip('编辑').last);
    await settle(tester);
    expect(find.text('HTTP'), findsWidgets);
    await tester.enterText(find.byKey(const Key('tunnel-domains')), '');
    await tester.enterText(find.byKey(const Key('tunnel-subdomain')), 'blog');
    await settle(tester);
    await tapKey(tester, 'tunnel-save');
    await settle(tester, 10);
    expect(s.tunnels.last['subdomain'], 'blog');
    expect(find.textContaining('http://blog.t.example.com:8080'), findsOneWidget);
    expect(c['id'], isNotNull);
  });

  test('publicAddress covers every kind', () {
    const node = NodeInfo(publicHost: 'h.com', vhostHttpPort: 80, vhostHttpsPort: 8443, subdomainHost: 't.com');
    Tunnel t(Map<String, Object?> j) => Tunnel.fromJson({'id': 'x', ...j});
    expect(publicAddress(t({'kind': 'tcp', 'remote_port': 6000}), node), 'h.com:6000');
    expect(publicAddress(t({'kind': 'tcp', 'remote_port': 6000}), null), ':6000');
    expect(publicAddress(t({'kind': 'udp'}), node), '自动分配');
    expect(publicAddress(t({'kind': 'http', 'custom_domains': ['a.com']}), node), 'http://a.com');
    expect(publicAddress(t({'kind': 'https', 'custom_domains': ['a.com']}), node), 'https://a.com:8443');
    expect(publicAddress(t({'kind': 'http', 'subdomain': 'x'}), node), 'http://x.t.com');
    expect(publicAddress(t({'kind': 'http', 'subdomain': 'x'}), const NodeInfo()), '未配置域名');
    expect(publicAddress(t({'kind': 'http', 'custom_domains': ['a.com']}), null), 'http://a.com');
    expect(publicAddress(t({'kind': 'https', 'custom_domains': ['a.com']}), const NodeInfo(vhostHttpsPort: 443)), 'https://a.com');
  });
}
