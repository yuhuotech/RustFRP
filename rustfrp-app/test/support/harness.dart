import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/app/app.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:rustfrp_app/features/settings/settings_view.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'fake_server.dart';

/// Builds the real app on top of a [FakeServer]. [tick] lets tests drive the live-refresh stream.
class Harness {
  Harness._(this.server, this.container, this.tick);
  final FakeServer server;
  final ProviderContainer container;
  final StreamController<int> tick;

  static Future<Harness> create(
    WidgetTester tester,
    FakeServer server, {
    Size size = const Size(1200, 800),
    bool sameOrigin = true,
    Map<String, Object> prefs = const {},
    bool login = true,
    List<dynamic> extraOverrides = const [],
    Future<bool> Function()? desktopStartupEnabled,
    Future<void> Function(bool)? desktopStartupToggle,
  }) async {
    SharedPreferences.setMockInitialValues(prefs);
    final p = await SharedPreferences.getInstance();
    tester.view.physicalSize = size;
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final tick = StreamController<int>.broadcast();
    addTearDown(tick.close);
    final container = ProviderContainer(
      retry: (_, _) => null,
      overrides: [
        prefsProvider.overrideWithValue(p),
        apiFactoryProvider.overrideWithValue((base) => server.api(base)),
        sameOriginProvider.overrideWithValue(sameOrigin),
        ...extraOverrides,
        tickProvider.overrideWith((ref) => tick.stream),
        if (desktopStartupEnabled != null)
          desktopStartupEnabledProvider.overrideWithValue(
            desktopStartupEnabled,
          ),
        if (desktopStartupToggle != null)
          desktopStartupToggleProvider.overrideWithValue(desktopStartupToggle),
      ],
    );
    addTearDown(container.dispose);
    server.loggedIn = login;
    // A previous app in the same test must be torn down completely (widget states would otherwise be reused).
    await tester.pumpWidget(const SizedBox());
    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const RustFrpApp(),
      ),
    );
    await settle(tester);
    return Harness._(server, container, tick);
  }
}

/// Lets futures, provider rebuilds and short animations finish (the app never settles on its own because of
/// the progress indicators / periodic tick).
Future<void> settle(WidgetTester tester, [int frames = 6]) async {
  for (var i = 0; i < frames; i++) {
    await tester.pump(const Duration(milliseconds: 60));
  }
}

Future<void> tapKey(WidgetTester tester, String key) async {
  await tester.tap(find.byKey(Key(key)));
  await settle(tester);
}

Future<void> tapText(WidgetTester tester, String text, {int index = 0}) async {
  await tester.tap(find.text(text).at(index));
  await settle(tester);
}
