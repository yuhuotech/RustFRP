import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart' show Size;
import 'package:window_manager/window_manager.dart';

/// Windows and Linux draw their own minimise / maximise / close buttons (the title bar is hidden for a cleaner
/// look); macOS keeps its native traffic lights.
bool get hasCustomWindowControls => windowShellActive && !Platform.isMacOS;

/// True only in the real app, after [initDesktopWindow] set up the window plugin. Widget tests run on desktop
/// hosts too, where the plugin does not exist, so UI code must not assume it.
bool get windowShellActive => debugWindowShell ?? (isDesktop && _initialized);

/// Test hook: pretend the desktop window shell is (not) active.
@visibleForTesting
bool? debugWindowShell;

bool get isDesktop =>
    !kIsWeb && (Platform.isMacOS || Platform.isWindows || Platform.isLinux);

bool _initialized = false;

/// Hidden title bar, sensible minimum size. Desktop only; a no-op elsewhere.
Future<void> initDesktopWindow() async {
  if (!isDesktop) return;
  await windowManager.ensureInitialized();
  await windowManager.waitUntilReadyToShow(
    const WindowOptions(
      size: Size(1180, 760),
      minimumSize: Size(360, 560),
      titleBarStyle: TitleBarStyle.hidden,
      title: 'RustFRP',
    ),
    () async {
      await windowManager.show();
      await windowManager.focus();
    },
  );
  _initialized = true;
}
