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
    loader.addFont(Future.value(ByteData.sublistView(File('web/fonts/NotoSansSC-$f.ttf').readAsBytesSync())));
  }
  await loader.load();
  final icons = FontLoader('packages/lucide_icons_flutter/Lucide');
  icons.addFont(Future.value(ByteData.sublistView(File(_lucidePath()).readAsBytesSync())));
  await icons.load();
}

String _lucidePath() {
  final cfg = File('.dart_tool/package_config.json').readAsStringSync();
  final m = RegExp(r'"name": "lucide_icons_flutter",\s*"rootUri": "([^"]+)"').firstMatch(cfg)!;
  final root = m.group(1)!.replaceFirst('file://', '');
  return '$root/assets/lucide.ttf';
}

Future<void> shot(WidgetTester tester, String dir, String name) async {
  await tester.runAsync(() async {
    final boundary = tester.firstRenderObject<RenderRepaintBoundary>(find.byType(RepaintBoundary).first);
    final image = await boundary.toImage(pixelRatio: 1);
    final bytes = (await image.toByteData(format: ui.ImageByteFormat.png))!.buffer.asUint8List();
    File('$dir/$name.png')
      ..createSync(recursive: true)
      ..writeAsBytesSync(bytes);
  });
}

void main() {
  final dir = Platform.environment['SCREENSHOTS'];
  testWidgets('screenshots', skip: dir == null, (tester) async {
    await tester.runAsync(loadFonts);
    final s = FakeServer();
    final nas = s.addClient('家里的 NAS', isOnline: true);
    final laptop = s.addClient('办公室 MacBook', os: 'macos', isOnline: true);
    s.addClient('测试服务器', os: 'windows', disabled: true);
    s.addTunnel(nas['id']! as String, 'ssh', local: 22, remote: 6000);
    s.addTunnel(nas['id']! as String, 'nas-web', local: 5000, remote: 6001);
    s.addTunnel(laptop['id']! as String, 'rdp', local: 3389, remote: 6002, state: 'error', error: 'listen on port 6002 failed: Address already in use');
    s.audit.add({'id': 2, 'ts': 1700000000000, 'actor': 'root', 'action': 'tunnel.create', 'target_kind': 'tunnel', 'target_id': 'abc', 'reason': '需要 SSH 远程登录', 'result': 'ok', 'ip': '203.0.113.9'});
    s.audit.add({'id': 1, 'ts': 1699990000000, 'actor': 'root', 'action': 'client.enroll', 'target_kind': 'client', 'target_id': 'def', 'reason': 'enroll token x', 'result': 'ok', 'ip': ''});

    for (final theme in ['light', 'dark']) {
      final h = await Harness.create(tester, s, prefs: {'theme_mode': theme});
      await tester.pump(const Duration(seconds: 1));
      await shot(tester, dir!, '$theme-overview');
      for (final (tip, name) in [('客户端', 'clients'), ('隧道', 'tunnels'), ('审计', 'audit'), ('设置', 'settings')]) {
        await tester.tap(find.byTooltip(tip));
        await settle(tester, 8);
        if (name == 'clients') h.container.read(selectedClientProvider.notifier).select(nas['id']! as String);
        await settle(tester, 8);
        await shot(tester, dir, '$theme-$name');
      }
      final n = await Harness.create(tester, s, size: const Size(390, 780), prefs: {'theme_mode': theme});
      await tester.pump(const Duration(seconds: 1));
      await shot(tester, dir, '$theme-narrow-overview');
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
