import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'app/app.dart';
import 'core/desktop.dart';
import 'core/desktop_lifecycle.dart';
import 'core/fonts.dart';
import 'core/notification_service.dart';
import 'core/store.dart';

Future<void> main(List<String> arguments) async {
  WidgetsFlutterBinding.ensureInitialized();
  await loadWebFonts();
  final startHidden = arguments.contains('--startup-to-tray');
  await initDesktopWindow(showAtStartup: !startHidden);
  final prefs = await SharedPreferences.getInstance();
  await DesktopAppLifecycle(prefs).initialize(startHidden: startHidden);
  await DesktopNotificationService.instance.initialize();
  runApp(
    ProviderScope(
      // Live data is re-fetched by the periodic tick anyway; no hidden exponential retries.
      retry: (_, _) => null,
      overrides: [
        prefsProvider.overrideWithValue(prefs),
        // The browser build is served by the controller itself: always talk to the same origin.
        sameOriginProvider.overrideWithValue(kIsWeb),
      ],
      child: const RustFrpApp(),
    ),
  );
}
