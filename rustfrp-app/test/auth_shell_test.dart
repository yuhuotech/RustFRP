import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/features/shell/shell.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

void main() {
  testWidgets('login page (same origin): hides the server field, shows errors, then enters the app', (tester) async {
    final s = FakeServer();
    await Harness.create(tester, s, login: false);
    expect(find.byKey(const Key('login-server')), findsNothing);
    expect(find.text('登录管理面板'), findsOneWidget);
    final submit = tester.widget<GestureDetector>(find.descendant(of: find.byKey(const Key('login-submit')), matching: find.byType(GestureDetector)).first);
    expect(submit.onTap, isNull, reason: 'login is disabled until both fields are filled');

    await tester.enterText(find.byKey(const Key('login-name')), 'root');
    await tester.enterText(find.byKey(const Key('login-password')), 'wrong');
    await settle(tester);
    await tapKey(tester, 'login-submit');
    expect(find.byKey(const Key('login-error')), findsOneWidget);
    expect(find.text('invalid user name or password'), findsOneWidget);

    await tester.enterText(find.byKey(const Key('login-password')), s.adminPassword);
    await settle(tester);
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await settle(tester, 10);
    expect(find.byType(AppShell), findsOneWidget);
    expect(find.text('概览'), findsWidgets);
  });

  testWidgets('login page (desktop): asks for the controller address and rejects a bad one', (tester) async {
    final s = FakeServer();
    await Harness.create(tester, s, login: false, sameOrigin: false);
    expect(find.byKey(const Key('login-server')), findsOneWidget);
    await tester.enterText(find.byKey(const Key('login-server')), 'not a url');
    await tester.enterText(find.byKey(const Key('login-name')), 'root');
    await tester.enterText(find.byKey(const Key('login-password')), s.adminPassword);
    await settle(tester);
    await tapKey(tester, 'login-submit');
    expect(find.textContaining('完整的服务器地址'), findsOneWidget);

    await tester.enterText(find.byKey(const Key('login-server')), 'https://frp.example.com');
    await settle(tester);
    await tapKey(tester, 'login-submit');
    await settle(tester, 10);
    expect(find.byType(AppShell), findsOneWidget);
  });

  testWidgets('a browser with a valid session cookie skips the login page', (tester) async {
    await Harness.create(tester, FakeServer());
    expect(find.byType(AppShell), findsOneWidget);
    expect(find.byKey(const Key('login-submit')), findsNothing);
  });

  testWidgets('wide layout: rail, list column and content; navigation switches sections', (tester) async {
    final s = FakeServer();
    s.addClient('nas', isOnline: true);
    await Harness.create(tester, s);
    // Rail items for an owner: overview, nodes, clients, tunnels, audit (+ settings at the bottom).
    for (final tip in ['概览', '节点', '客户端', '隧道', '审计', '设置']) {
      expect(find.byTooltip(tip), findsOneWidget, reason: tip);
    }
    // Overview has no list column; clients does.
    expect(find.text('在线客户端'), findsOneWidget);
    await tester.tap(find.byTooltip('客户端'));
    await settle(tester);
    expect(find.byKey(const Key('client-row-id1')), findsOneWidget);
    expect(find.text('选择一台客户端'), findsOneWidget);

    await tester.tap(find.byTooltip('节点'));
    await settle(tester);
    expect(find.text('内置节点'), findsWidgets);
    await tester.tap(find.byTooltip('隧道'));
    await settle(tester);
    expect(find.text('还没有隧道'), findsOneWidget);
    await tester.tap(find.byTooltip('审计'));
    await settle(tester);
    expect(find.text('暂无审计记录'), findsOneWidget);
    await tester.tap(find.byTooltip('设置'));
    await settle(tester);
    expect(find.text('账号'), findsWidgets);
    // The logo returns to the overview.
    await tester.tap(find.byTooltip('返回首页'));
    await settle(tester);
    expect(find.text('在线客户端'), findsOneWidget);
  });

  testWidgets('role gating: a viewer does not get the audit page or write buttons', (tester) async {
    final s = FakeServer(role: 'viewer');
    s.addClient('nas');
    await Harness.create(tester, s);
    expect(find.byTooltip('审计'), findsNothing);
    expect(find.text('接入新客户端'), findsNothing, reason: 'enrolling needs admin');
  });

  testWidgets('connection indicator reflects the controller state', (tester) async {
    final s = FakeServer();
    final h = await Harness.create(tester, s);
    expect(find.byTooltip('已连接'), findsOneWidget);
    s.offline = true;
    h.tick.add(1);
    await settle(tester, 10);
    expect(find.byTooltip('无法连接控制器'), findsOneWidget);
  });

  testWidgets('narrow layout: bottom navigation, list → detail push, back button', (tester) async {
    final s = FakeServer();
    final c = s.addClient('nas', isOnline: true);
    s.addTunnel(c['id']! as String, 'ssh');
    await Harness.create(tester, s, size: const Size(390, 800));
    // Bottom bar with all six destinations.
    for (final label in ['概览', '节点', '客户端', '隧道', '审计', '设置']) {
      expect(find.text(label), findsWidgets, reason: label);
    }
    await tester.tap(find.text('客户端').last);
    await settle(tester);
    expect(find.byKey(Key('client-row-${c['id']}')), findsOneWidget);
    await tester.tap(find.byKey(Key('client-row-${c['id']}')));
    await settle(tester, 10);
    expect(find.text('设备信息'), findsOneWidget);
    expect(find.byTooltip('返回'), findsOneWidget);
    await tester.tap(find.byTooltip('返回'));
    await settle(tester, 10);
    expect(find.byKey(Key('client-row-${c['id']}')), findsOneWidget);

    await tester.tap(find.text('隧道').last);
    await settle(tester);
    expect(find.byKey(const Key('new-tunnel')), findsOneWidget);
    expect(find.text('全部'), findsOneWidget, reason: 'narrow screens show the filter chips');
    await tester.tap(find.text('设置').last);
    await settle(tester);
    await tester.tap(find.byKey(const Key('settings-about')));
    await settle(tester, 10);
    expect(find.text('控制器版本'), findsOneWidget);
  });

  testWidgets('dark theme renders the same shell', (tester) async {
    await Harness.create(tester, FakeServer(), prefs: {'theme_mode': 'dark'});
    final ctx = tester.element(find.byType(AppShell));
    expect(Theme.of(ctx).brightness, Brightness.dark);
  });
}
