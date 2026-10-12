import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
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
  await tester.enterText(
    find.widgetWithText(TextField, '操作理由（必填，会写入审计）').first,
    reason,
  );
  await settle(tester);
}

void main() {
  clientTagAndRestartTests();
  testWidgets(
    'clients list: rows show state, selecting shows details and tunnels',
    (tester) async {
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
      expect(find.text('203.0.113.7'), findsOneWidget);
      expect(find.text('隧道（1）'), findsOneWidget);
      expect(find.byKey(const Key('client-new-tunnel')), findsOneWidget);
      expect(find.text('在线'), findsWidgets);

      await tapKey(tester, 'client-back');
      await tester.tap(find.byKey(Key('client-row-${b['id']}')));
      await settle(tester);
      expect(find.text('这台客户端还没有隧道。'), findsOneWidget);
      expect(find.text('最近 IP'), findsOneWidget);
      expect(find.text('已停用'), findsWidgets);
      expect(
        find.byTooltip('强制下线'),
        findsNothing,
        reason: 'offline devices cannot be kicked',
      );
    },
  );

  testWidgets('clients: empty state offers enrollment to admins only', (
    tester,
  ) async {
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
    h.container
        .read(selectedClientProvider.notifier)
        .select(c['id']! as String);
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

  testWidgets(
    'disable, enable, kick and delete require a reason and update the list',
    (tester) async {
      final s = FakeServer();
      final c = s.addClient('nas', isOnline: true);
      s.addTunnel(c['id']! as String, 'ssh');
      final h = await Harness.create(tester, s);
      h.container
          .read(selectedClientProvider.notifier)
          .select(c['id']! as String);
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
      expect(
        s.requests.where((r) => r.method == 'PATCH').last.body,
        contains('设备丢失'),
      );

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
    },
  );

  testWidgets('a rejected write shows the server message as a toast', (
    tester,
  ) async {
    final s = FakeServer();
    final c = s.addClient('nas');
    final h = await Harness.create(tester, s);
    h.container
        .read(selectedClientProvider.notifier)
        .select(c['id']! as String);
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

  testWidgets('selected client that no longer exists, and load errors', (
    tester,
  ) async {
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

  testWidgets('client config import previews changes before applying', (
    tester,
  ) async {
    final s = FakeServer();
    final c = s.addClient('nas');
    await Harness.create(tester, s);
    await openClients(tester);
    await tester.tap(find.byKey(Key('client-row-${c['id']}')));
    await settle(tester);
    await tapKey(tester, 'client-config-import');
    await tapKey(tester, 'config-import-preview');
    expect(find.text('请填写不超过 1 MiB 的 frpc 配置文本。'), findsOneWidget);
    await tester.enterText(
      find.byKey(const Key('config-import-text')),
      '[[proxies]]\nname="ssh"\ntype="tcp"',
    );
    s.failNext = true;
    await tapKey(tester, 'config-import-preview');
    await settle(tester, 10);
    expect(find.text('internal error'), findsOneWidget);
    await tapKey(tester, 'config-import-preview');
    await settle(tester, 10);
    expect(find.text('将应用 1 条隧道'), findsOneWidget);
    expect(find.textContaining('兼容提示'), findsOneWidget);
    final apply = find.byKey(const Key('config-import-apply'));
    expect(tester.widget<LButton>(apply).onPressed, isNull);
    await tester.enterText(
      find.byKey(const Key('config-import-reason')),
      '按批准的配置迁移',
    );
    await tester.tap(find.byKey(const Key('config-import-ack')));
    await settle(tester);
    expect(tester.widget<LButton>(apply).onPressed, isNotNull);
    await tester.tap(apply);
    await settle(tester, 10);
    expect(s.configImports, hasLength(1));
    expect(find.text('配置已导入'), findsOneWidget);
    expect(s.configImports.single['digest'], 'preview-digest');
    expect(s.configImports.single['accept_warnings'], true);
    expect(s.configImports.single['reason'], '按批准的配置迁移');
  });

  testWidgets('client config can be exported and copied', (tester) async {
    final s = FakeServer();
    final client = s.addClient('export-box');
    await Harness.create(tester, s);
    String? copied;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          copied = (call.arguments as Map)['text'] as String;
        }
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );
    await openClients(tester);
    await tester.tap(find.byKey(Key('client-row-${client['id']}')));
    await settle(tester);

    await tapKey(tester, 'client-config-export');
    await settle(tester, 10);
    expect(find.byKey(const Key('config-export-text')), findsOneWidget);
    expect(find.textContaining('name = "ssh"'), findsOneWidget);
    await tapText(tester, '复制');
    expect(copied, contains('name = "ssh"'));
    expect(find.text('配置已复制'), findsOneWidget);
    await tapText(tester, '关闭');
    expect(find.byKey(const Key('config-export-text')), findsNothing);
  });

  testWidgets('migration reports when there is no alternate node', (
    tester,
  ) async {
    final s = FakeServer();
    final client = s.addClient('single-node');
    await Harness.create(tester, s);
    await openClients(tester);
    await tester.tap(find.byKey(Key('client-row-${client['id']}')));
    await settle(tester);
    await tapKey(tester, 'client-migrate-node');
    await settle(tester, 10);
    expect(find.text('没有可迁移的目标节点'), findsOneWidget);
  });

  testWidgets('tunnels page: list, filter, toggle, edit, delete', (
    tester,
  ) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    final id = c['id']! as String;
    final ssh = s.addTunnel(id, 'ssh', remote: 6000);
    s.addTunnel(
      id,
      'web',
      local: 80,
      remote: 6001,
      state: 'error',
      error: 'listen on port 6001 failed',
    );
    s.addTunnel(
      id,
      'db',
      local: 5432,
      remote: 0,
      state: 'stopped',
      enabled: false,
    );
    await Harness.create(tester, s);
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    expect(find.text('ssh'), findsOneWidget);
    expect(find.text('listen on port 6001 failed'), findsOneWidget);
    expect(find.textContaining('frp.example.com:6000'), findsOneWidget);
    expect(find.textContaining('自动分配'), findsOneWidget);
    expect(find.text('已停用'), findsOneWidget);

    // Filters stay in the workspace header.
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

  testWidgets('create a tunnel: validation, server conflicts, success', (
    tester,
  ) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    s.addTunnel(c['id']! as String, 'taken', remote: 6000);
    await Harness.create(tester, s);
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    await tapKey(tester, 'new-tunnel');
    expect(find.text('新建隧道'), findsWidgets);

    // Save is disabled until the form is valid.
    GestureDetector saveGesture() => tester.widget<GestureDetector>(
      find
          .descendant(
            of: find.byKey(const Key('tunnel-save')),
            matching: find.byType(GestureDetector),
          )
          .first,
    );
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
    expect(
      find.textContaining('already used'),
      findsOneWidget,
      reason: 'the server rejects the conflicting port; the dialog stays open',
    );
    expect(find.text('新建隧道'), findsWidgets);

    await tester.enterText(find.byKey(const Key('tunnel-remote-port')), '');
    await settle(tester);
    await tapKey(tester, 'tunnel-save');
    await settle(tester, 10);
    expect(s.tunnels.where((t) => t['name'] == 'ssh'), hasLength(1));
    expect(s.tunnels.last['remote_port'], 0);
  });

  testWidgets('new tunnel is disabled with no clients; a viewer cannot edit', (
    tester,
  ) async {
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

  testWidgets('tunnel editor from a client page preselects that client', (
    tester,
  ) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    final h = await Harness.create(tester, s);
    h.container
        .read(selectedClientProvider.notifier)
        .select(c['id']! as String);
    await openClients(tester);
    await tapKey(tester, 'client-new-tunnel');
    await tapKey(tester, 'tunnel-kind');
    await tester.tap(find.text('TCPMUX（CONNECT 按域名分流）').last);
    await settle(tester);
    await tester.enterText(find.byKey(const Key('tunnel-name')), 'ssh-connect');
    await tester.enterText(find.byKey(const Key('tunnel-local-port')), '22');
    await tester.ensureVisible(find.byKey(const Key('tunnel-domains')));
    await tester.enterText(
      find.byKey(const Key('tunnel-domains')),
      'ssh.example.test',
    );
    await tester.enterText(
      find.byKey(const Key('tunnel-max-connections')),
      '12',
    );
    await tester.ensureVisible(find.byKey(const Key('tunnel-use-encryption')));
    await tester.tap(find.byKey(const Key('tunnel-use-encryption')));
    await tester.tap(find.byKey(const Key('tunnel-use-compression')));
    await settle(tester);
    await tapKey(tester, 'tunnel-save');
    await settle(tester, 10);
    expect(s.tunnels.single['client_id'], c['id']);
    expect(s.tunnels.single['kind'], 'tcpmux');
    expect(s.tunnels.single['multiplexer'], 'httpconnect');
    expect(s.tunnels.single['max_connections'], 12);
    expect(s.tunnels.single['use_encryption'], true);
    expect(s.tunnels.single['use_compression'], true);
    expect(
      find.textContaining('CONNECT ssh.example.test:7443'),
      findsOneWidget,
    );
  });

  testWidgets(
    'create UDP and HTTP tunnels; domains are required for web types; addresses are shown as URLs',
    (tester) async {
      final s = FakeServer();
      final c = s.addClient('nas', isOnline: true);
      await Harness.create(tester, s, size: const Size(1200, 1600));
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
      await tester.enterText(
        find.byKey(const Key('tunnel-remote-port')),
        '5353',
      );
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
      GestureDetector save() => tester.widget<GestureDetector>(
        find
            .descendant(
              of: find.byKey(const Key('tunnel-save')),
              matching: find.byType(GestureDetector),
            )
            .first,
      );
      expect(save().onTap, isNull);
      await tester.enterText(
        find.byKey(const Key('tunnel-domains')),
        'WWW.example.com, b.example.com',
      );
      await tester.enterText(
        find.byKey(const Key('tunnel-rewrite')),
        'internal.lan',
      );
      // HTTP 高级选项：路径前缀必须以 / 开头；请求头每行 Name: value。
      await tester.enterText(find.byKey(const Key('tunnel-locations')), 'api');
      await settle(tester);
      expect(save().onTap, isNull, reason: 'location without a leading slash');
      await tester.enterText(
        find.byKey(const Key('tunnel-locations')),
        '/api, /rpc',
      );
      await tester.enterText(find.byKey(const Key('tunnel-headers')), 'oops');
      await settle(tester);
      expect(save().onTap, isNull, reason: 'header line without a colon');
      await tester.enterText(
        find.byKey(const Key('tunnel-headers')),
        'X-Env: prod\n\nX-Team:  core ',
      );
      await tester.enterText(
        find.byKey(const Key('tunnel-response-headers')),
        'bad line',
      );
      await settle(tester);
      expect(save().onTap, isNull, reason: 'response header without a colon');
      await tester.enterText(
        find.byKey(const Key('tunnel-response-headers')),
        'X-Frame-Options: deny',
      );
      await tester.enterText(
        find.byKey(const Key('tunnel-http-user')),
        'alice',
      );
      await tester.enterText(
        find.byKey(const Key('tunnel-http-password')),
        's3cret',
      );
      await tester.enterText(
        find.byKey(const Key('tunnel-route-user')),
        'alice',
      );
      await tester.ensureVisible(
        find.byKey(const Key('tunnel-proxy-protocol')),
      );
      await tester.tap(find.byKey(const Key('tunnel-proxy-protocol')));
      await settle(tester);
      await tester.tap(find.text('v2（二进制）').last);
      await settle(tester);
      await settle(tester);
      await tester.ensureVisible(find.byKey(const Key('tunnel-bandwidth')));
      await tester.enterText(find.byKey(const Key('tunnel-bandwidth')), 'abc');
      await settle(tester);
      expect(save().onTap, isNull, reason: 'non-numeric limit is rejected');
      await tester.enterText(find.byKey(const Key('tunnel-bandwidth')), '512');
      await tester.tap(find.byKey(const Key('tunnel-bandwidth-mode')));
      await settle(tester);
      await tester.tap(find.text('在服务端限速').last);
      await settle(tester);
      // 负载均衡组与健康检查。
      await tester.ensureVisible(find.byKey(const Key('tunnel-lb-group')));
      await tester.enterText(find.byKey(const Key('tunnel-lb-group')), ' web ');
      await tester.enterText(find.byKey(const Key('tunnel-lb-key')), 'sekret');
      await tester.ensureVisible(find.byKey(const Key('tunnel-health-kind')));
      await tester.tap(find.byKey(const Key('tunnel-health-kind')));
      await settle(tester);
      await tester.tap(find.text('HTTP 2xx').last);
      await settle(tester);
      await tester.ensureVisible(find.byKey(const Key('tunnel-health-path')));
      await tester.enterText(
        find.byKey(const Key('tunnel-health-path')),
        'healthz',
      );
      await settle(tester);
      expect(save().onTap, isNull, reason: 'health path without a slash');
      await tester.enterText(
        find.byKey(const Key('tunnel-health-path')),
        '/healthz',
      );
      await tester.enterText(
        find.byKey(const Key('tunnel-health-timeout')),
        '0',
      );
      await settle(tester);
      expect(save().onTap, isNull, reason: 'timeout out of range');
      await tester.enterText(
        find.byKey(const Key('tunnel-health-timeout')),
        '2',
      );
      await tester.enterText(find.byKey(const Key('tunnel-health-max')), '3');
      await tester.enterText(
        find.byKey(const Key('tunnel-health-interval')),
        '5',
      );
      await settle(tester);
      expect(save().onTap, isNotNull);
      await tapKey(tester, 'tunnel-save');
      await settle(tester, 10);
      final web = s.tunnels.last;
      expect(web['locations'], ['/api', '/rpc']);
      expect(web['request_headers'], {'X-Env': 'prod', 'X-Team': 'core'});
      expect(web['response_headers'], {'X-Frame-Options': 'deny'});
      expect(web['http_user'], 'alice');
      expect(web['route_by_http_user'], 'alice');
      expect(web['http_password_set'], true);
      expect(web['proxy_protocol'], 2);
      expect(web['bandwidth_bps'], 512 * 1024);
      expect(web['bandwidth_server'], true);
      expect(web['lb_group'], 'web');
      expect(web['lb_group_key_set'], true);
      expect(web['health_check'], {
        'kind': 'http',
        'timeout_secs': 2,
        'max_failed': 3,
        'interval_secs': 5,
        'path': '/healthz',
        'headers': <String, String>{},
      });
      expect(s.httpPasswords[web['id']], 's3cret');
      expect(web['kind'], 'http');
      expect(web['custom_domains'], ['WWW.example.com', 'b.example.com']);
      expect(web['host_header_rewrite'], 'internal.lan');
      expect(
        find.textContaining('http://WWW.example.com:8080'),
        findsOneWidget,
      );
      expect(find.textContaining('(+1)'), findsOneWidget);

      // Editing keeps the type fixed and updates domains.
      await tester.tap(find.byTooltip('编辑').last);
      await settle(tester);
      expect(find.text('HTTP'), findsWidgets);
      await tester.enterText(find.byKey(const Key('tunnel-domains')), '');
      await tester.enterText(find.byKey(const Key('tunnel-subdomain')), 'blog');
      await settle(tester);
      // 密码留空 = 保持原样（请求里不带 http_password）。
      expect(find.text('已设置，留空保持不变'), findsOneWidget);
      await tapKey(tester, 'tunnel-save');
      await settle(tester, 10);
      expect(s.tunnels.last['subdomain'], 'blog');
      expect(s.httpPasswords[web['id']], 's3cret');
      expect(s.tunnels.last['locations'], ['/api', '/rpc']);

      // 再次编辑：换新密码；然后选择清除密码。
      await tester.tap(find.byTooltip('编辑').last);
      await settle(tester);
      await tester.enterText(
        find.byKey(const Key('tunnel-http-password')),
        'rotated',
      );
      await settle(tester);
      await tapKey(tester, 'tunnel-save');
      await settle(tester, 10);
      expect(s.httpPasswords[web['id']], 'rotated');
      await tester.tap(find.byTooltip('编辑').last);
      await settle(tester);
      await tester.ensureVisible(
        find.byKey(const Key('tunnel-clear-password')),
      );
      await tapKey(tester, 'tunnel-clear-password');
      await tapKey(tester, 'tunnel-save');
      await settle(tester, 10);
      expect(s.httpPasswords.containsKey(web['id']), isFalse);
      expect(s.tunnels.last['http_password_set'], false);
      expect(
        find.textContaining('http://blog.t.example.com:8080'),
        findsOneWidget,
      );
      expect(c['id'], isNotNull);
    },
  );

  test('publicAddress covers every kind', () {
    const node = NodeInfo(
      publicHost: 'h.com',
      vhostHttpPort: 80,
      vhostHttpsPort: 8443,
      tcpmuxHttpConnectPort: 7443,
      subdomainHost: 't.com',
    );
    Tunnel t(Map<String, Object?> j) => Tunnel.fromJson({'id': 'x', ...j});
    expect(
      publicAddress(t({'kind': 'tcp', 'remote_port': 6000}), node),
      'h.com:6000',
    );
    expect(
      publicAddress(t({'kind': 'tcp', 'remote_port': 6000}), null),
      ':6000',
    );
    expect(publicAddress(t({'kind': 'udp'}), node), '自动分配');
    expect(
      publicAddress(
        t({
          'kind': 'tcpmux',
          'custom_domains': ['ssh.example.test'],
        }),
        node,
      ),
      'CONNECT ssh.example.test:7443',
    );
    expect(
      publicAddress(
        t({
          'kind': 'http',
          'custom_domains': ['a.com'],
        }),
        node,
      ),
      'http://a.com',
    );
    expect(
      publicAddress(
        t({
          'kind': 'https',
          'custom_domains': ['a.com'],
        }),
        node,
      ),
      'https://a.com:8443',
    );
    expect(
      publicAddress(t({'kind': 'http', 'subdomain': 'x'}), node),
      'http://x.t.com',
    );
    expect(
      publicAddress(t({'kind': 'http', 'subdomain': 'x'}), const NodeInfo()),
      '未配置域名',
    );
    expect(
      publicAddress(
        t({
          'kind': 'http',
          'custom_domains': ['a.com'],
        }),
        null,
      ),
      'http://a.com',
    );
    expect(
      publicAddress(
        t({
          'kind': 'https',
          'custom_domains': ['a.com'],
        }),
        const NodeInfo(vhostHttpsPort: 443),
      ),
      'https://a.com',
    );
  });
  testWidgets('batch selected tunnels enable disable and failure recovery', (
    tester,
  ) async {
    final s = FakeServer();
    final client = s.addClient('batch-device');
    final a = s.addTunnel(client['id']! as String, 'batch-a');
    final b = s.addTunnel(client['id']! as String, 'batch-b');
    await Harness.create(tester, s, size: const Size(1200, 1000));
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    await tapKey(tester, 'select-tunnel-${a['id']}');
    await tapKey(tester, 'select-tunnel-${b['id']}');
    expect(find.text('已选 2 条'), findsOneWidget);
    await tapKey(tester, 'batch-disable');
    expect(a['enabled'], false);
    expect(b['enabled'], false);
    final request = s.requests.lastWhere(
      (r) => r.url.path.endsWith('/tunnels/batch'),
    );
    expect(request.body, contains('批量停用'));
    expect(find.text('已选 2 条'), findsNothing);
    await tapKey(tester, 'select-tunnel-${a['id']}');
    await tapKey(tester, 'batch-enable');
    expect(a['enabled'], true);
    await tapKey(tester, 'select-tunnel-${a['id']}');
    await tapKey(tester, 'select-tunnel-${a['id']}');
    expect(find.text('已选 1 条'), findsNothing);
    await tapKey(tester, 'select-tunnel-${b['id']}');
    await tapText(tester, '清除选择');
    expect(find.text('已选 1 条'), findsNothing);
    await tapKey(tester, 'select-tunnel-${a['id']}');
    s.failNext = true;
    await tapKey(tester, 'batch-disable');
    expect(find.text('internal error'), findsOneWidget);
    expect(a['enabled'], true);
    expect(find.text('已选 1 条'), findsOneWidget);
    await tapKey(tester, 'batch-disable');
    expect(a['enabled'], false);
    expect(tester.takeException(), isNull);
  });
  testWidgets('service templates fill editable defaults', (tester) async {
    final s = FakeServer();
    s.addClient('template-device');
    await Harness.create(tester, s, size: const Size(1200, 1100));
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    await tapKey(tester, 'new-tunnel');
    for (final entry in [
      ('SSH', 'SSH', '22'),
      ('远程桌面 RDP', 'RDP', '3389'),
      ('Web', 'Web', '80'),
      ('MySQL', 'MySQL', '3306'),
      ('PostgreSQL', 'PostgreSQL', '5432'),
    ]) {
      await tapKey(tester, 'tunnel-template');
      await tapText(
        tester,
        entry.$1,
        index: find.text(entry.$1).evaluate().length - 1,
      );
      expect(
        tester
            .widget<LTextField>(find.byKey(const Key('tunnel-name')))
            .controller!
            .text,
        entry.$2,
      );
      expect(
        tester
            .widget<LTextField>(find.byKey(const Key('tunnel-local-port')))
            .controller!
            .text,
        entry.$3,
      );
    }
    await tapKey(tester, 'tunnel-template');
    await tapText(tester, '自定义');
    expect(
      tester
          .widget<LTextField>(find.byKey(const Key('tunnel-name')))
          .controller!
          .text,
      'PostgreSQL',
    );
    await tester.enterText(find.byKey(const Key('tunnel-name')), 'my-service');
    expect(
      tester
          .widget<LTextField>(find.byKey(const Key('tunnel-name')))
          .controller!
          .text,
      'my-service',
    );
  });
}

void clientTagAndRestartTests() {
  testWidgets('tags can be edited and an online agent can be restarted', (
    tester,
  ) async {
    final s = FakeServer();
    final a = s.addClient('nas', isOnline: true);
    final b = s.addClient('offline-box');
    await Harness.create(tester, s);
    await openClients(tester);
    await tester.tap(find.byKey(Key('client-row-${a['id']}')));
    await settle(tester);

    await tapKey(tester, 'client-tags');
    await tester.enterText(
      find.byKey(const Key('tags-field')),
      'prod, nas，  ,web',
    );
    await tapKey(tester, 'tags-save');
    await settle(tester, 10);
    expect(a['tags'], ['prod', 'nas', 'web']);
    expect(find.text('prod'), findsWidgets);

    await tester.tap(find.byTooltip('重启 agent'));
    await settle(tester);
    await fillReason(tester, '升级后重启');
    await tester.tap(find.widgetWithText(LButton, '重启').last);
    await settle(tester, 10);
    expect(s.restarts, [a['id']]);
    expect(find.text('已发送重启指令'), findsOneWidget);

    // 离线设备没有「重启」入口；列表副标题带标签。
    await tapKey(tester, 'client-back');
    expect(find.textContaining('prod / nas / web'), findsOneWidget);
    await tester.tap(find.byKey(Key('client-row-${b['id']}')));
    await settle(tester);
    expect(find.byTooltip('重启 agent'), findsNothing);
  });

  testWidgets('client limit, edge migration and signed Agent update controls', (
    tester,
  ) async {
    final s = FakeServer();
    final client = s.addClient('nas', isOnline: true, maxConnections: 32);
    final edge = s.addNode('edge-west', host: 'west.example.test');
    await Harness.create(tester, s);
    await openClients(tester);
    await tester.tap(find.byKey(Key('client-row-${client['id']}')));
    await settle(tester);

    await tapKey(tester, 'client-connection-limit');
    expect(
      tester.widget<TextField>(find.byType(TextField).last).controller!.text,
      '32',
    );
    await tester.enterText(
      find.byKey(const Key('client-max-connections')),
      'bad',
    );
    await tester.tap(find.widgetWithText(LButton, '保存'));
    await settle(tester, 10);
    expect(find.text('连接上限须为 0 到 1000000 的整数'), findsOneWidget);
    await tapKey(tester, 'client-connection-limit');
    await tester.enterText(
      find.byKey(const Key('client-max-connections')),
      '1000001',
    );
    await tester.tap(find.widgetWithText(LButton, '保存'));
    await settle(tester, 10);
    expect(find.text('连接上限须为 0 到 1000000 的整数'), findsOneWidget);
    await tapKey(tester, 'client-connection-limit');
    await tester.enterText(
      find.byKey(const Key('client-max-connections')),
      '128',
    );
    await tester.tap(find.widgetWithText(LButton, '保存'));
    await settle(tester, 10);
    expect(client['max_connections'], 128);

    await tapKey(tester, 'client-connection-limit');
    await tester.enterText(find.byKey(const Key('client-max-connections')), '');
    await tester.tap(find.widgetWithText(LButton, '保存'));
    await settle(tester, 10);
    expect(client['max_connections'], 0);
    final unchangedCount = s.requests.length;
    await tapKey(tester, 'client-connection-limit');
    await tapText(tester, '取消');
    expect(s.requests.length, unchangedCount);

    await tapKey(tester, 'client-migrate-node');
    await settle(tester);
    await tester.tap(find.byKey(Key('move-node-${edge['id']}')));
    await settle(tester);
    await fillReason(tester, '就近接入');
    await tester.tap(find.widgetWithText(LButton, '迁移').last);
    await settle(tester, 10);
    expect(s.migrations.single['node_id'], edge['id']);

    await tapKey(tester, 'client-update-agent');
    await tester.enterText(
      find.byKey(const Key('agent-update-version')),
      'v1.2.3',
    );
    await tester.tap(find.widgetWithText(LButton, '下一步'));
    await settle(tester);
    await fillReason(tester, '安全更新');
    await tester.tap(find.widgetWithText(LButton, '下发更新'));
    await settle(tester, 10);
    expect(s.agentUpdates.single['version'], 'v1.2.3');
  });

  testWidgets(
    'narrow client detail exposes the limit, migration and update actions',
    (tester) async {
      tester.view.physicalSize = const Size(390, 844);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);

      final s = FakeServer();
      final client = s.addClient('mobile-narrow', isOnline: true);
      final edge = s.addNode('edge-narrow', host: 'narrow.example.test');
      await Harness.create(tester, s);
      await openClients(tester);
      await tester.tap(find.byKey(Key('client-row-${client['id']}')));
      await settle(tester);

      await tapKey(tester, 'client-connection-limit');
      await tester.enterText(
        find.byKey(const Key('client-max-connections')),
        '16',
      );
      await tester.tap(find.widgetWithText(LButton, '保存'));
      await settle(tester, 10);
      expect(client['max_connections'], 16);

      await tapKey(tester, 'client-migrate-node');
      await settle(tester);
      await tester.tap(find.byKey(Key('move-node-${edge['id']}')));
      await settle(tester);
      await fillReason(tester, '移动端就近接入');
      await tester.tap(find.widgetWithText(LButton, '迁移').last);
      await settle(tester, 10);
      expect(s.migrations.single['node_id'], edge['id']);

      await tapKey(tester, 'client-update-agent');
      await tester.enterText(
        find.byKey(const Key('agent-update-version')),
        'v1.2.4',
      );
      await tester.tap(find.widgetWithText(LButton, '下一步'));
      await settle(tester);
      await fillReason(tester, '移动端更新');
      await tester.tap(find.widgetWithText(LButton, '下发更新'));
      await settle(tester, 10);
      expect(s.agentUpdates.single['version'], 'v1.2.4');
    },
  );
}
