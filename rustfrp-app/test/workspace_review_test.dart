import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:rustfrp_app/shared/workspace_widgets.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

void main() {
  for (final width in [1440.0, 390.0]) {
    for (final theme in ['light', 'dark']) {
      testWidgets('management workspaces at $width px in $theme theme', (
        tester,
      ) async {
        final s = FakeServer();
        final device = s.addClient('长名称客户端用于验证表格和详情', isOnline: true);
        device['tags'] = ['生产环境', '上海'];
        s.addTunnel(device['id']! as String, '业务系统', state: 'running');
        s.audit.add({
          'id': 1,
          'ts': 1700000000000,
          'actor': 'root',
          'action': 'tunnel.create',
          'target_kind': 'tunnel',
          'target_id': 't1',
          'reason': '发布业务入口',
          'result': 'ok',
          'ip': '203.0.113.7',
        });
        final h = await Harness.create(
          tester,
          s,
          size: Size(width, 1000),
          prefs: {'theme_mode': theme},
        );
        for (final section in [
          Section.clients,
          Section.tunnels,
          Section.audit,
          Section.settings,
        ]) {
          h.container.read(sectionProvider.notifier).select(section);
          if (section == Section.settings) {
            h.container.read(selectedSettingProvider.notifier).select('admins');
          }
          await settle(tester, 10);
          if (section == Section.settings && width < 1000) {
            await tapKey(tester, 'settings-admins');
          }
          expect(tester.takeException(), isNull, reason: '$section at $width');
          if (width >= 1000 || section == Section.audit) {
            expect(find.byType(ResourceTable), findsOneWidget);
            final row = find.byType(ResourceRow).first;
            expect(tester.getSize(row).height, greaterThanOrEqualTo(64));
          }
        }
        // Mobile keeps all administrator actions reachable by horizontal scrolling.
        expect(find.byKey(const Key('new-admin')), findsOneWidget);
        if (width < 1000) {
          await tester.drag(find.byType(ResourceTable), const Offset(-600, 0));
          await settle(tester);
          expect(tester.takeException(), isNull);
        }
      });
    }
  }
}
