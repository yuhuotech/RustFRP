import 'dart:convert';
// Visual review helper: renders the main screens with the real fonts and writes PNGs.
//   SCREENSHOTS=/tmp/rustfrp-shots flutter test test/screenshots_test.dart
// Skipped in normal runs (no env var). Not a golden test: the output is for humans.
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/store.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

Future<void> loadFonts() async {
  final loader = FontLoader('NotoSansSC');
  for (final f in ['Regular', 'Bold']) {
    loader.addFont(
      Future.value(
        ByteData.sublistView(
          File('web/fonts/NotoSansSC-$f.ttf').readAsBytesSync(),
        ),
      ),
    );
  }
  await loader.load();
  // Widget tests substitute Ahem for the normal Latin font; load SDK Roboto
  // as well so numbers / hostnames are reviewable instead of black blocks.
  final packages = jsonDecode(
    File('.dart_tool/package_config.json').readAsStringSync(),
  ) as Map<String, dynamic>;
  final flutter = (packages['packages'] as List)
      .cast<Map<String, dynamic>>()
      .firstWhere((p) => p['name'] == 'flutter');
  final sdk = Uri.parse('${flutter['rootUri']}/')
      .resolve('../../')
      .toFilePath();
  final latin = FontLoader('Roboto');
  for (final weight in ['Regular', 'Medium', 'Bold']) {
    latin.addFont(
      Future.value(
        ByteData.sublistView(
          File('$sdk/bin/cache/artifacts/material_fonts/Roboto-$weight.ttf')
              .readAsBytesSync(),
        ),
      ),
    );
  }
  await latin.load();
  final mono = FontLoader('monospace');
  final monoFile = File('/System/Library/Fonts/Menlo.ttc');
  mono.addFont(
    Future.value(
      ByteData.sublistView(
        (monoFile.existsSync()
                ? monoFile
                : File(
                    '$sdk/bin/cache/artifacts/material_fonts/Roboto-Regular.ttf',
                  ))
            .readAsBytesSync(),
      ),
    ),
  );
  await mono.load();

  final icons = FontLoader('packages/lucide_icons_flutter/Lucide');
  icons.addFont(
    Future.value(ByteData.sublistView(File(_lucidePath()).readAsBytesSync())),
  );
  await icons.load();
}

String _lucidePath() {
  final cfg = File('.dart_tool/package_config.json').readAsStringSync();
  final m = RegExp(r'"name": "lucide_icons_flutter",\s*"rootUri": "([^"]+)"')
      .firstMatch(cfg)!;
  final root = m.group(1)!.replaceFirst('file://', '');
  return '$root/assets/lucide.ttf';
}

Future<void> shot(WidgetTester tester, String dir, String name) async {
  await tester.runAsync(
    () => precacheImage(
      const AssetImage('assets/brand/rustfrp-logo-128.png'),
      tester.element(find.byType(MaterialApp)),
    ),
  );
  await tester.pump();
  await tester.runAsync(() async {
    final boundary = tester.firstRenderObject<RenderRepaintBoundary>(
      find.byType(RepaintBoundary).first,
    );
    final image = await boundary.toImage(pixelRatio: 1);
    final bytes = (await image.toByteData(format: ui.ImageByteFormat.png))!
        .buffer
        .asUint8List();
    File('$dir/$name.png')
      ..createSync(recursive: true)
      ..writeAsBytesSync(bytes);
  });
}

void main() {
  final dir = Platform.environment['SCREENSHOTS'];
  testWidgets('screenshots', skip: dir == null, (tester) async {
    await tester.runAsync(loadFonts);
    final s = FakeServer()
      ..trafficWindowEnd =
          DateTime.now().millisecondsSinceEpoch ~/ 3600000 * 3600000;
    for (var hour = 0; hour < 24; hour++) {
      if (hour == 10) continue;
      s.traffic.add({
        'timestamp': s.trafficWindowEnd - (23 - hour) * 3600000,
        'bytes_in': (hour % 7 + 1) * 1500000,
        'bytes_out': (hour % 5 + 1) * 7000000,
        'connections_total': hour + 2,
        'gap': hour == 14 || hour == 23,
      });
    }
    final nas = s.addClient('家里的 NAS', isOnline: true);
    final laptop = s.addClient('办公室 MacBook', os: 'macos', isOnline: true);
    s.addClient('测试服务器', os: 'windows', disabled: true);
    s.addTunnel(nas['id']! as String, 'ssh', local: 22, remote: 6000);
    s.addTunnel(nas['id']! as String, 'nas-web', local: 5000, remote: 6001);
    s.addTunnel(
      laptop['id']! as String,
      'rdp',
      local: 3389,
      remote: 6002,
      state: 'error',
      error: 'listen on port 6002 failed: Address already in use',
    );
    s.audit.add({
      'id': 2,
      'ts': 1700000000000,
      'actor': 'root',
      'action': 'tunnel.create',
      'target_kind': 'tunnel',
      'target_id': 'abc',
      'reason': '需要 SSH 远程登录',
      'result': 'ok',
      'ip': '203.0.113.9',
    });
    s.audit.add({
      'id': 1,
      'ts': 1699990000000,
      'actor': 'root',
      'action': 'client.enroll',
      'target_kind': 'client',
      'target_id': 'def',
      'reason': 'enroll token x',
      'result': 'ok',
      'ip': '',
    });

    for (final theme in ['light', 'dark']) {
      final h = await Harness.create(tester, s, prefs: {'theme_mode': theme});
      await tester.pump(const Duration(seconds: 1));
      await shot(tester, dir!, '$theme-overview');
      h.container.read(sidebarCollapsedProvider.notifier).toggle();
      await settle(tester);
      await shot(tester, dir, '$theme-overview-collapsed');
      h.container.read(sidebarCollapsedProvider.notifier).toggle();
      await settle(tester);
      for (final (tip, name) in [
        ('客户端', 'clients'),
        ('隧道', 'tunnels'),
        ('审计', 'audit'),
        ('设置', 'settings'),
      ]) {
        await tester.tap(find.byTooltip(tip));
        await settle(tester, 8);
        if (name == 'clients') {
          h.container
              .read(selectedClientProvider.notifier)
              .select(nas['id']! as String);
        }
        await settle(tester, 8);
        await shot(tester, dir, '$theme-$name');
      }
      final n = await Harness.create(
        tester,
        s,
        size: const Size(390, 780),
        prefs: {'theme_mode': theme},
      );
      await tester.pump(const Duration(seconds: 1));
      await shot(tester, dir, '$theme-narrow-overview');
      await tester.scrollUntilVisible(
        find.byKey(const Key('traffic-chart')),
        120,
        scrollable: find.byType(Scrollable).last,
      );
      await settle(tester);
      await tester.ensureVisible(find.text('流量历史'));
      await settle(tester);
      await shot(tester, dir, '$theme-narrow-traffic');
      await tester.tap(find.text('隧道').last);
      await settle(tester, 8);
      await shot(tester, dir, '$theme-narrow-tunnels');
      expect(n.server, s);
    }
    final login = await Harness.create(tester, FakeServer(), login: false);
    await tester.pump(const Duration(seconds: 1));
    await shot(tester, dir!, 'login');
    expect(login.server.loggedIn, isFalse);
  });
}
