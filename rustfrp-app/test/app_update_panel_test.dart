import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/app_update_install.dart';
import 'package:rustfrp_app/core/app_updater.dart';

import 'support/fake_server.dart';
import 'support/harness.dart';

class FixedAppUpdater extends AppUpdateNotifier {
  FixedAppUpdater(this.initial);
  final AppUpdateState initial;
  @override
  AppUpdateState build() => initial;
}

const _release = AppRelease(
  tag: 'v0.2.0',
  version: '0.2.0',
  notes: 'Security fixes and safer connection limits.',
  pageUrl: 'https://github.com/owner/rustfrp/releases/tag/v0.2.0',
  assets: [
    AppReleaseAsset('rustfrp-macos-arm64.zip', 'https://example.test/pkg', 100),
  ],
);

Future<void> _openAbout(
  WidgetTester tester,
  FakeServer server,
  AppUpdateState state,
) async {
  await Harness.create(
    tester,
    server,
    extraOverrides: [
      appUpdateProvider.overrideWith(() => FixedAppUpdater(state)),
      appUpdateConfigProvider.overrideWithValue(
        const AppUpdateConfig(
          repository: 'owner/rustfrp',
          publicKeyHex: '1111111111111111111111111111111111111111111111111111111111111111',
          autoCheck: false,
        ),
      ),
    ],
  );
  await tester.tap(find.byTooltip('系统设置'));
  await settle(tester);
  await tapKey(tester, 'settings-about');
}

void main() {
  testWidgets(
    'update panel shows signed release details, progress and source choices',
    (tester) async {
      await _openAbout(
        tester,
        FakeServer(),
        AppUpdateState(
          phase: AppUpdatePhase.downloading,
          latest: _release,
          plan: AppInstallPlan.fake('rustfrp-macos-arm64.zip'),
          progress: 0.5,
          source: AppUpdateSource.mainland,
          mirrorUsed: 'https://gh-proxy.com',
        ),
      );
      expect(find.text('正在下载 50%'), findsOneWidget);
      expect(find.textContaining('当前加速源'), findsOneWidget);
      expect(find.textContaining('Security fixes'), findsOneWidget);
      expect(find.byType(LinearProgressIndicator), findsOneWidget);
      expect(find.text('取消'), findsOneWidget);
      expect(find.text('国内加速'), findsOneWidget);
      expect(find.text('GitHub 官方'), findsOneWidget);
    },
  );

  testWidgets(
    'manual installation guidance is shown when this package cannot self-update',
    (tester) async {
      await _openAbout(
        tester,
        FakeServer(),
        AppUpdateState(
          latest: _release,
          plan: AppInstallPlan.manual('请手动安装 Linux 更新包'),
        ),
      );
      expect(find.text('发现新版本 0.2.0'), findsOneWidget);
      expect(find.text('请手动安装 Linux 更新包'), findsOneWidget);
      expect(find.text('Release 页面'), findsOneWidget);
      expect(find.text('下载并安装'), findsNothing);
    },
  );
}
