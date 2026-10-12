import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:rustfrp_app/shared/widgets.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

void main() {
  testWidgets('enroll wizard: name → command (copy) → waits → device joins', (
    tester,
  ) async {
    final s = FakeServer();
    final h = await Harness.create(tester, s);
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

    await tapText(tester, '接入新客户端');
    expect(find.text('生成命令'), findsOneWidget);
    expect(
      tester.widget<LButton>(find.byKey(const Key('enroll-create'))).onPressed,
      isNull,
      reason: 'a name is required',
    );
    await tester.enterText(find.byKey(const Key('enroll-name')), '家里的 NAS');
    await settle(tester);
    // Pick another expiry.
    await tester.tap(find.text('1 小时'));
    await settle(tester);
    await tester.tap(find.text('15 分钟').last);
    await settle(tester);
    await tapKey(tester, 'enroll-create');

    expect(find.text('在目标机器上执行'), findsOneWidget);
    expect(
      s.requests
          .where(
            (r) => r.method == 'POST' && r.url.path.endsWith('/enroll-tokens'),
          )
          .single
          .body,
      contains('"ttl_minutes":15'),
    );
    final cmd = tester.widget<SelectableText>(
      find.descendant(
        of: find.byKey(const Key('enroll-command')),
        matching: find.byType(SelectableText),
      ),
    );
    expect(cmd.data, contains('curl -fsSL https://frp.example.com/install.sh'));
    expect(cmd.data, contains('rfrp_secret_token'));
    expect(find.text('等待设备上线…'), findsOneWidget);

    // Copy button.
    await tester.tap(find.byTooltip('复制'));
    await settle(tester);
    expect(copied, contains('rfrp_secret_token'));
    expect(find.text('已复制'), findsOneWidget);

    // Switch to the direct command.
    await tester.tap(find.text('Linux / macOS（一行安装）'));
    await settle(tester);
    await tester.tap(find.text('已安装 agent，直接运行').last);
    await settle(tester);
    expect(
      tester
          .widget<SelectableText>(
            find.descendant(
              of: find.byKey(const Key('enroll-command')),
              matching: find.byType(SelectableText),
            ),
          )
          .data,
      startsWith('rustfrp-agent --mode client'),
    );

    // Unrelated enrolment must never complete this wizard.
    s.addClient('unrelated', isOnline: true);
    h.tick.add(1);
    await settle(tester, 10);
    expect(find.byKey(const Key('enroll-joined')), findsNothing);
    final joined = s.addClient('家里的 NAS', isOnline: false);
    s.tokens.single['client_id'] = joined['id'];
    s.tokens.single['used_at'] = 1;
    h.tick.add(2);
    await settle(tester, 10);
    expect(find.byKey(const Key('enroll-joined')), findsNothing);
    s.online.add(joined['id']! as String);
    h.tick.add(1);
    await settle(tester, 10);
    expect(find.byKey(const Key('enroll-joined')), findsOneWidget);
    await tapText(tester, '查看客户端');
    expect(h.container.read(selectedClientProvider), joined['id']);
    expect(h.container.read(sectionProvider), Section.clients);
  });

  testWidgets(
    'enroll wizard without a public URL only offers the direct command; errors are toasts',
    (tester) async {
      final s = FakeServer()..publicUrl = '';
      await Harness.create(tester, s);
      await tapText(tester, '接入新客户端');
      await tester.enterText(find.byKey(const Key('enroll-name')), 'box');
      await settle(tester);
      s.failNext = true;
      await tapKey(tester, 'enroll-create');
      expect(find.text('internal error'), findsOneWidget);
      expect(find.text('在目标机器上执行'), findsNothing);

      await tapKey(tester, 'enroll-create');
      expect(find.text('在目标机器上执行'), findsOneWidget);
      expect(
        tester
            .widget<SelectableText>(
              find.descendant(
                of: find.byKey(const Key('enroll-command')),
                matching: find.byType(SelectableText),
              ),
            )
            .data,
        startsWith('rustfrp-agent'),
      );
      await tapText(tester, '完成');
      expect(find.text('在目标机器上执行'), findsNothing);
      // Cancelling the first step closes it.
      await tapText(tester, '接入新客户端');
      await tapText(tester, '取消');
      expect(find.text('生成命令'), findsNothing);
    },
  );

  testWidgets('overview: metrics, node card, recent clients, empty hint', (
    tester,
  ) async {
    final s = FakeServer();
    await Harness.create(tester, s);
    await tester.drag(find.byType(ListView).first, const Offset(0, -500));
    await settle(tester);
    expect(find.textContaining('还没有客户端。点击右上角'), findsOneWidget);
    expect(find.text('内置节点（builtin）'), findsOneWidget);
    expect(find.textContaining('frp.example.com:7000'), findsOneWidget);
    expect(find.text('6000-6100, 7000'), findsOneWidget);

    final c = s.addClient('nas', isOnline: true);
    s.addClient('laptop');
    s.addTunnel(c['id']! as String, 'ssh');
    final h = await Harness.create(tester, s);
    h.tick.add(1);
    await settle(tester, 10);
    final m = tester.widget<Text>(
      find.descendant(
        of: find.byKey(const Key('metric-clients')),
        matching: find.text('1'),
      ),
    );
    expect(m.data, '1');
    expect(find.text('共 2 台'), findsOneWidget);
    expect(find.text('3.0 KiB'), findsOneWidget); // 24h 流量
    expect(
      find.text('内存 5.0 MiB · 运行 1 天 1 小时 · CPU 累计 12.3s'),
      findsOneWidget,
    );
    await tester.drag(find.byType(ListView).first, const Offset(0, -500));
    await settle(tester);
    expect(find.text('在线'), findsWidgets);
    expect(find.textContaining('最近在线'), findsOneWidget);
  });

  testWidgets('node page', (tester) async {
    final s = FakeServer()..publicHost = '';
    await Harness.create(tester, s);
    await tester.tap(find.byTooltip('节点'));
    await settle(tester);
    expect(find.textContaining('未配置'), findsWidgets);
    await tapKey(tester, 'node-row-builtin');
    expect(find.text('客户端接入端口'), findsOneWidget);
    expect(find.textContaining('内置节点随控制器一起运行'), findsOneWidget);
    // 内置节点没有重命名 / 停用 / 删除。
    expect(find.byTooltip('重命名'), findsNothing);
    await tapKey(tester, 'node-back');
    expect(find.byKey(const Key('new-node')), findsOneWidget);
  });

  testWidgets('audit page lists entries with labels, results and reasons', (
    tester,
  ) async {
    final s = FakeServer();
    s.audit.addAll([
      {
        'id': 2,
        'ts': 1700000000000,
        'actor': 'root',
        'action': 'tunnel.create',
        'target_kind': 'tunnel',
        'target_id': 't1',
        'reason': '需要 SSH',
        'result': 'ok',
        'ip': '1.2.3.4',
      },
      {
        'id': 1,
        'ts': 1700000000000,
        'actor': 'ops',
        'action': 'permission.denied',
        'target_kind': 'api',
        'target_id': '',
        'reason': 'insufficient role',
        'result': 'denied',
        'ip': '',
      },
      {
        'id': 0,
        'ts': 1700000000000,
        'actor': 'x',
        'action': 'custom.thing',
        'target_kind': '',
        'target_id': '',
        'reason': '',
        'result': 'ok',
        'ip': '',
      },
    ]);
    final h = await Harness.create(tester, s);
    await tester.tap(find.byTooltip('审计'));
    await settle(tester);
    expect(find.text('创建隧道'), findsOneWidget);
    expect(find.text('权限不足'), findsOneWidget);
    expect(
      find.text('custom.thing'),
      findsOneWidget,
      reason: 'unknown actions are shown raw',
    );
    expect(find.text('成功'), findsNWidgets(2));
    expect(find.text('拒绝'), findsOneWidget);
    expect(find.textContaining('理由：需要 SSH'), findsOneWidget);
    expect(find.textContaining('root · 1.2.3.4 · tunnel t1'), findsOneWidget);
    await tester.tap(find.byTooltip('刷新'));
    await settle(tester);
    expect(
      h.server.requests.where((r) => r.url.path.endsWith('/audit')).length,
      greaterThan(1),
    );
  });

  testWidgets('settings: account, appearance, tokens (revoke), admins, about', (
    tester,
  ) async {
    final s = FakeServer();
    await Harness.create(tester, s);
    // Seed a pending and a used token through the API the UI uses.
    await s.api().login(s.adminName, s.adminPassword);
    final h = await Harness.create(tester, s);
    await h.container.read(actionsProvider).createToken('office', 60, 'r');
    s.tokens.add({
      'id': 'used1',
      'name': 'old',
      'created_by': 'root',
      'created_at': 1,
      'expires_at': 2,
      'used_at': 3,
      'client_id': 'c',
      'revoked_at': null,
    });
    s.tokens.add({
      'id': 'rev1',
      'name': 'revoked',
      'created_by': 'root',
      'created_at': 1,
      'expires_at': 4102444800000,
      'used_at': null,
      'client_id': null,
      'revoked_at': 5,
    });
    s.tokens.add({
      'id': 'exp1',
      'name': 'expired',
      'created_by': 'root',
      'created_at': 1,
      'expires_at': 2,
      'used_at': null,
      'client_id': null,
      'revoked_at': null,
    });

    await tester.tap(find.byTooltip('设置'));
    await settle(tester);
    expect(find.text('root'), findsWidgets);
    expect(find.text('所有者'), findsWidgets);

    await tapKey(tester, 'settings-appearance');
    await tapKey(tester, 'theme-dark');
    expect(h.container.read(themeModeProvider), ThemeMode.dark);
    await tapKey(tester, 'theme-light');
    expect(h.container.read(themeModeProvider), ThemeMode.light);
    await tapKey(tester, 'theme-system');

    await tapKey(tester, 'settings-tokens');
    h.tick.add(1);
    await settle(tester, 10);
    for (final t in ['待使用', '已使用', '已撤销', '已过期']) {
      expect(find.text(t), findsOneWidget, reason: t);
    }
    await tapText(tester, '撤销');
    await tester.enterText(
      find.widgetWithText(TextField, '操作理由（必填，会写入审计）').first,
      '误创建',
    );
    await settle(tester);
    await tester.tap(find.widgetWithText(LButton, '撤销').last);
    await settle(tester, 10);
    expect(s.tokens.where((t) => t['revoked_at'] != null), hasLength(2));

    await tapKey(tester, 'settings-admins');
    h.tick.add(2);
    await settle(tester, 10);
    expect(find.text('ops'), findsOneWidget);
    expect(find.text('已禁用'), findsOneWidget);
    expect(find.textContaining('至少保留一位启用的所有者'), findsOneWidget);
    // 自己没有角色 / 启停控件；其他管理员有。
    expect(find.byKey(const Key('admin-role-root')), findsNothing);
    expect(find.byKey(const Key('admin-toggle-ops')), findsOneWidget);

    // 启用被禁用的管理员（需要理由）。
    await tapKey(tester, 'admin-toggle-ops');
    await tester.enterText(
      find.widgetWithText(TextField, '操作理由（必填，会写入审计）').first,
      '恢复',
    );
    await settle(tester);
    await tester.tap(find.widgetWithText(LButton, '确认').last);
    await settle(tester, 10);
    expect(s.extraAdmins.first['disabled_at'], isNull);
    expect(find.text('已禁用'), findsNothing);

    // 新增管理员：密码不足 10 位时不能提交。
    await tapKey(tester, 'new-admin');
    await tester.enterText(find.byKey(const Key('admin-name')), 'newbie');
    await tester.enterText(find.byKey(const Key('admin-password')), 'short');
    await tester.enterText(find.byKey(const Key('admin-reason')), '入职');
    await settle(tester);
    expect(
      tester.widget<LButton>(find.byKey(const Key('admin-submit'))).onPressed,
      isNull,
    );
    await tester.enterText(
      find.byKey(const Key('admin-password')),
      'a long enough password',
    );
    await settle(tester);
    await tapKey(tester, 'admin-submit');
    await settle(tester, 10);
    expect(s.extraAdmins.any((a) => a['name'] == 'newbie'), isTrue);
    expect(find.text('newbie'), findsOneWidget);

    // 改角色（经确认理由）与重置密码。
    await tapKey(tester, 'admin-passwd-newbie');
    await tester.enterText(
      find.byKey(const Key('admin-password')),
      'another long password',
    );
    await tester.enterText(find.byKey(const Key('admin-reason')), '重置');
    await settle(tester);
    await tapKey(tester, 'admin-submit');
    await settle(tester, 10);
    expect(
      s.requests.where(
        (r) => r.method == 'PATCH' && r.url.path.contains('/admins/'),
      ),
      isNotEmpty,
    );

    await tapKey(tester, 'settings-about');
    expect(find.text('RustFRP'), findsWidgets);
    expect(find.text('应用版本'), findsOneWidget);
    expect(find.byKey(const Key('app-update-check')), findsOneWidget);
    await tapKey(tester, 'app-update-check');
    expect(find.textContaining('Release 仓库地址'), findsOneWidget);

    await tapKey(tester, 'settings-account');
    await tapKey(tester, 'logout');
    await settle(tester, 10);
    expect(find.byKey(const Key('login-submit')), findsOneWidget);
  });

  testWidgets('desktop preferences toggle startup, tray and notifications', (
    tester,
  ) async {
    final s = FakeServer();
    final h = await Harness.create(
      tester,
      s,
      desktopStartupEnabled: () async => false,
      desktopStartupToggle: (enabled) async => expect(enabled, true),
    );
    await tester.tap(find.byTooltip('设置'));
    await settle(tester);
    await tapKey(tester, 'settings-appearance');
    await settle(tester, 12);
    expect(find.text('桌面行为'), findsOneWidget);
    final switches = find.byType(LSwitch);
    expect(switches, findsNWidgets(3));

    await tester.tap(switches.at(0));
    await settle(tester);
    await tester.tap(find.byType(LSwitch).at(1));
    await settle(tester);
    await tester.tap(find.byType(LSwitch).at(2));
    await settle(tester);
    expect(h.container.read(prefsProvider).getBool('close_to_tray'), false);
    expect(
      h.container.read(prefsProvider).getBool('notifications_enabled'),
      false,
    );
    expect(tester.takeException(), isNull);
  });

  testWidgets('desktop startup errors stay visible and recoverable', (
    tester,
  ) async {
    final s = FakeServer();
    await Harness.create(
      tester,
      s,
      desktopStartupEnabled: () async => false,
      desktopStartupToggle: (_) async {
        throw const FileSystemException('settings directory is read-only');
      },
    );
    await tester.tap(find.byTooltip('设置'));
    await settle(tester);
    await tapKey(tester, 'settings-appearance');
    await settle(tester, 12);
    await tester.tap(find.byType(LSwitch).first);
    await settle(tester);
    expect(find.textContaining('开机启动设置失败'), findsOneWidget);
  });

  testWidgets(
    'settings hides pages the role may not use; empty tokens and load errors',
    (tester) async {
      final s = FakeServer(role: 'operator');
      await Harness.create(tester, s);
      await tester.tap(find.byTooltip('设置'));
      await settle(tester);
      expect(find.byKey(const Key('settings-tokens')), findsNothing);
      expect(find.byKey(const Key('settings-admins')), findsNothing);
      expect(find.byKey(const Key('settings-about')), findsOneWidget);

      final o = FakeServer();
      final h = await Harness.create(tester, o);
      await tester.tap(find.byTooltip('设置'));
      await settle(tester);
      o.failNext = true;
      await tapKey(tester, 'settings-tokens');
      await settle(tester, 10);
      expect(find.text('internal error'), findsOneWidget);
      await tapText(tester, '重试');
      await settle(tester, 10);
      expect(find.textContaining('还没有创建过接入令牌'), findsOneWidget);
      expect(h.container.read(sectionProvider), Section.settings);
    },
  );

  testWidgets(
    'a session expiring in the background returns to the login page',
    (tester) async {
      final s = FakeServer();
      final h = await Harness.create(tester, s);
      s.loggedIn = false;
      h.tick.add(1);
      await settle(tester, 10);
      expect(find.byKey(const Key('login-submit')), findsOneWidget);
    },
  );
  testWidgets('API Token creation one-time value and revocation', (
    tester,
  ) async {
    final s = FakeServer();
    await Harness.create(tester, s, size: const Size(1200, 1000));
    await tester.tap(find.byTooltip('设置'));
    await settle(tester);
    await tapKey(tester, 'settings-api_tokens');
    expect(find.text('暂无 API Token'), findsOneWidget);
    await tapKey(tester, 'new-api-token');
    expect(
      tester
          .widget<LButton>(find.byKey(const Key('create-api-token')))
          .onPressed,
      isNull,
    );
    await tester.enterText(find.byKey(const Key('api-token-name')), 'CI');
    await settle(tester);
    await tapKey(tester, 'api-token-scope');
    await tapText(tester, '隧道管理');
    s.failNext = true;
    await tapKey(tester, 'create-api-token');
    expect(find.text('internal error'), findsOneWidget);
    await tapKey(tester, 'create-api-token');
    expect(find.text('保存 API Token'), findsOneWidget);
    expect(find.text('rfrp_automation_test'), findsOneWidget);
    await tapText(tester, '关闭');
    expect(find.text('rfrp_automation_test'), findsNothing);
    expect(s.apiTokens.single['scope'], 'tunnels');
    await tapKey(tester, 'revoke-api-token-${s.apiTokens.single['id']}');
    await tester.enterText(
      find.widgetWithText(TextField, '操作理由（必填，会写入审计）').first,
      'retire',
    );
    await settle(tester);
    await tapText(tester, '撤销', index: find.text('撤销').evaluate().length - 1);
    expect(find.text('已撤销'), findsOneWidget);
  });
  testWidgets('controller log snapshots empty loaded and retry', (
    tester,
  ) async {
    final s = FakeServer();
    final h = await Harness.create(tester, s, size: const Size(1200, 1000));
    await tester.tap(find.byTooltip('设置'));
    await settle(tester);
    await tapKey(tester, 'settings-logs');
    expect(find.text('暂无日志'), findsOneWidget);
    s.controllerLogs = ['controller started', '[redacted]'];
    await tapText(tester, '刷新');
    expect(find.text('controller started'), findsOneWidget);
    s.failNext = true;
    await tapText(tester, '刷新');
    h.container.invalidate(controllerLogsProvider);
    await settle(tester);
    expect(find.text('controller started'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });
}
