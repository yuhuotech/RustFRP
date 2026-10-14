import 'dart:ui' show SemanticsAction;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/features/shell/shell.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:rustfrp_app/shared/widgets.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

void main() {
  testWidgets(
    'login page (same origin): hides the server field, shows errors, then enters the app',
    (tester) async {
      final s = FakeServer();
      await Harness.create(tester, s, login: false);
      expect(find.byKey(const Key('login-server')), findsNothing);
      expect(find.text('登录管理面板'), findsOneWidget);
      final submit = tester.widget<GestureDetector>(
        find
            .descendant(
              of: find.byKey(const Key('login-submit')),
              matching: find.byType(GestureDetector),
            )
            .first,
      );
      expect(
        submit.onTap,
        isNull,
        reason: 'login is disabled until both fields are filled',
      );

      await tester.enterText(find.byKey(const Key('login-name')), 'root');
      await tester.enterText(find.byKey(const Key('login-password')), 'wrong');
      await settle(tester);
      await tapKey(tester, 'login-submit');
      expect(find.byKey(const Key('login-error')), findsOneWidget);
      expect(find.text('invalid user name or password'), findsOneWidget);

      await tester.enterText(
        find.byKey(const Key('login-password')),
        s.adminPassword,
      );
      await settle(tester);
      await tester.testTextInput.receiveAction(TextInputAction.done);
      await settle(tester, 10);
      expect(find.byType(AppShell), findsOneWidget);
      expect(find.text('概览'), findsWidgets);
    },
  );

  testWidgets(
    'login page (desktop): asks for the controller address and rejects a bad one',
    (tester) async {
      final s = FakeServer();
      await Harness.create(tester, s, login: false, sameOrigin: false);
      expect(find.byKey(const Key('login-server')), findsOneWidget);
      await tester.enterText(
        find.byKey(const Key('login-server')),
        'not a url',
      );
      await tester.enterText(find.byKey(const Key('login-name')), 'root');
      await tester.enterText(
        find.byKey(const Key('login-password')),
        s.adminPassword,
      );
      await settle(tester);
      await tapKey(tester, 'login-submit');
      expect(find.textContaining('完整的服务器地址'), findsOneWidget);

      await tester.enterText(
        find.byKey(const Key('login-server')),
        'https://frp.example.com',
      );
      await settle(tester);
      await tapKey(tester, 'login-submit');
      await settle(tester, 10);
      expect(find.byType(AppShell), findsOneWidget);
    },
  );

  testWidgets('a browser with a valid session cookie skips the login page', (
    tester,
  ) async {
    await Harness.create(tester, FakeServer());
    expect(find.byType(AppShell), findsOneWidget);
    expect(find.byKey(const Key('login-submit')), findsNothing);
  });

  testWidgets(
    'wide layout: sidebar and full-width workspace; navigation switches sections',
    (tester) async {
      final s = FakeServer();
      s.addClient('nas', isOnline: true);
      await Harness.create(tester, s);
      // Rail items for an owner: overview, nodes, clients, tunnels, audit (+ settings at the bottom).
      for (final tip in ['概览', '节点管理', '客户端', '隧道管理', '审计日志', '系统设置']) {
        expect(find.byTooltip(tip), findsOneWidget, reason: tip);
      }
      // Overview has no list column; clients does.
      expect(find.text('在线客户端'), findsOneWidget);
      await tester.tap(find.byTooltip('客户端'));
      await settle(tester);
      expect(find.byKey(const Key('client-row-id1')), findsOneWidget);
      expect(find.text('选择一台客户端'), findsNothing);
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, 160);
      await tapKey(tester, 'client-row-id1');
      expect(find.text('设备信息'), findsOneWidget);
      await tapKey(tester, 'client-back');
      expect(find.byKey(const Key('client-row-id1')), findsOneWidget);

      await tester.tap(find.byTooltip('节点管理'));
      await settle(tester);
      expect(find.text('内置节点'), findsWidgets);
      await tester.tap(find.byTooltip('隧道管理'));
      await settle(tester);
      expect(find.text('还没有隧道'), findsOneWidget);
      await tester.tap(find.byTooltip('审计日志'));
      await settle(tester);
      expect(find.text('暂无审计记录'), findsOneWidget);
      await tester.tap(find.byTooltip('系统设置'));
      await settle(tester);
      expect(find.text('账号'), findsWidgets);
      // The logo returns to the overview.
      await tester.tap(find.byTooltip('返回首页'));
      await settle(tester);
      expect(find.text('在线客户端'), findsOneWidget);
    },
  );

  testWidgets(
    'sidebar collapses without losing navigation, selection or saved preference',
    (tester) async {
      final semantics = tester.ensureSemantics();
      final h = await Harness.create(tester, FakeServer());
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, 160);
      final sidebar = find.byKey(const Key('app-sidebar'));
      final menuRows = find.descendant(
        of: sidebar,
        matching: find.byType(HoverRow),
      );
      final overviewRow = tester.getRect(menuRows.at(0));
      final nodeRow = tester.getRect(menuRows.at(1));
      expect(overviewRow.height, 40);
      expect(nodeRow.top - overviewRow.bottom, 6);
      final overviewLabel = tester.widget<Text>(
        find.descendant(of: sidebar, matching: find.text('概览')),
      );
      final clientsLabel = tester.widget<Text>(
        find.descendant(of: sidebar, matching: find.text('客户端')),
      );
      expect(overviewLabel.style!.fontWeight, FontWeight.w500);
      expect(clientsLabel.style!.fontWeight, FontWeight.w400);
      final brand = find.descendant(
        of: sidebar,
        matching: find.byType(BrandLogo),
      );
      expect(tester.widget<BrandLogo>(brand).size, 22);
      final firstMenuIcon = find
          .descendant(of: menuRows.first, matching: find.byType(Icon))
          .first;
      expect(tester.getTopLeft(brand).dx, tester.getTopLeft(firstMenuIcon).dx);
      final title = tester.widget<Text>(
        find.descendant(of: sidebar, matching: find.text('RustFRP')),
      );
      expect(title.style!.fontSize, 13);
      expect(title.style!.fontWeight, FontWeight.w400);
      expect(tester.getTopLeft(brand).dy, greaterThanOrEqualTo(24));
      await tapKey(tester, 'sidebar-toggle');
      expect(tester.getSize(menuRows.first).height, 40);
      expect(
        find.descendant(of: sidebar, matching: find.byType(BrandLogo)),
        findsNothing,
      );
      expect(
        find.descendant(of: sidebar, matching: find.text('RustFRP')),
        findsNothing,
      );
      expect(
        tester.getCenter(find.byKey(const Key('sidebar-toggle'))).dx,
        tester.getCenter(sidebar).dx,
      );
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, 72);
      expect(find.byTooltip('展开菜单'), findsOneWidget);
      expect(
        h.container.read(prefsProvider).getBool('sidebar_collapsed'),
        true,
      );
      await tester.tap(find.byTooltip('系统设置'));
      await settle(tester);
      expect(find.byKey(const Key('settings-appearance')), findsOneWidget);
      final toggleNode = tester.getSemantics(
        find.byKey(const Key('sidebar-toggle')),
      );
      expect(toggleNode.label, '展开菜单');
      toggleNode.owner!.performAction(toggleNode.id, SemanticsAction.tap);
      await settle(tester);
      expect(find.byTooltip('收起菜单'), findsOneWidget);
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, 160);
      expect(
        h.container.read(prefsProvider).getBool('sidebar_collapsed'),
        false,
      );
      await tester.tap(find.byTooltip('返回首页'));
      await settle(tester);
      expect(find.text('在线客户端'), findsOneWidget);
      await Harness.create(
        tester,
        FakeServer(),
        prefs: {'sidebar_collapsed': true, 'theme_mode': 'dark'},
      );
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, 72);
      expect(find.byTooltip('概览'), findsOneWidget);
      semantics.dispose();
    },
  );

  testWidgets(
    'role gating: a viewer does not get the audit page or write buttons',
    (tester) async {
      final s = FakeServer(role: 'viewer');
      s.addClient('nas');
      await Harness.create(tester, s);
      expect(find.byTooltip('审计日志'), findsNothing);
      expect(
        find.text('接入新客户端'),
        findsNothing,
        reason: 'enrolling needs admin',
      );
    },
  );

  testWidgets('connection indicator reflects the controller state', (
    tester,
  ) async {
    final s = FakeServer();
    final h = await Harness.create(tester, s);
    expect(find.byTooltip('已连接'), findsOneWidget);
    s.offline = true;
    h.tick.add(1);
    await settle(tester, 10);
    expect(find.byTooltip('无法连接控制器'), findsOneWidget);
  });

  testWidgets(
    'narrow layout: bottom navigation, list → detail push, back button',
    (tester) async {
      final s = FakeServer();
      final c = s.addClient('nas', isOnline: true);
      s.addTunnel(c['id']! as String, 'ssh');
      await Harness.create(tester, s, size: const Size(390, 800));
      // Bottom bar with all six destinations.
      for (final label in ['概览', '节点管理', '客户端', '隧道管理', '审计日志', '系统设置']) {
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

      await tester.tap(find.text('隧道管理').last);
      await settle(tester);
      expect(find.byKey(const Key('new-tunnel')), findsOneWidget);
      expect(
        find.text('全部'),
        findsOneWidget,
        reason: 'narrow screens show the filter chips',
      );
      await tester.tap(find.text('系统设置').last);
      await settle(tester);
      await tester.tap(find.byKey(const Key('settings-about')));
      await settle(tester, 10);
      expect(find.text('控制器版本'), findsOneWidget);
    },
  );

  testWidgets(
    'narrow node list opens details and retains retry / back navigation',
    (tester) async {
      final server = FakeServer();
      final h = await Harness.create(
        tester,
        server,
        size: const Size(390, 800),
      );
      server.failNext = true;
      await tester.tap(find.text('节点管理').last);
      await settle(tester);
      expect(find.text('重试'), findsOneWidget);
      await tester.tap(find.text('重试'));
      await settle(tester);
      expect(find.text('内置节点'), findsOneWidget);
      await tester.tap(find.text('内置节点'));
      await settle(tester);
      expect(find.text('客户端接入端口'), findsOneWidget);
      await tester.tap(find.byTooltip('返回'));
      await settle(tester);
      server.publicHost = '';
      h.container.invalidate(nodesProvider);
      await settle(tester);
      expect(find.textContaining('未配置公网地址'), findsOneWidget);
    },
  );

  testWidgets('dark theme renders the same shell', (tester) async {
    await Harness.create(tester, FakeServer(), prefs: {'theme_mode': 'dark'});
    final ctx = tester.element(find.byType(AppShell));
    expect(Theme.of(ctx).brightness, Brightness.dark);
  });
}
