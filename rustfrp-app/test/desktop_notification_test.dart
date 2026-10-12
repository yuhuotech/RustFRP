import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tray_manager/tray_manager.dart';
import 'package:rustfrp_app/core/desktop.dart';
import 'package:rustfrp_app/core/desktop_lifecycle_io.dart';
import 'package:rustfrp_app/core/notification_service_io.dart';
import 'package:shared_preferences/shared_preferences.dart';

class FakeNotifications implements DesktopNotificationDriver {
  int initializations = 0;
  int permissionRequests = 0;
  final shown = <(int, String, String, NotificationDetails)>[];
  bool failShow = false;

  @override
  Future<void> initialize(InitializationSettings settings) async {
    initializations++;
    expect(settings.android, isNotNull);
    expect(settings.linux, isNotNull);
  }

  @override
  Future<void> requestPermission() async {
    permissionRequests++;
  }

  @override
  Future<void> show({
    required int id,
    required String title,
    required String body,
    required NotificationDetails details,
  }) async {
    if (failShow) throw StateError('notification service unavailable');
    shown.add((id, title, body, details));
  }
}

class FakeTray implements TrayIcon {
  FutureOr<void> Function(TrayIconEvent)? onEvent;
  bool disposed = false;

  @override
  ListenerId addListener(FutureOr<void> Function(TrayIconEvent) callback) {
    onEvent = callback;
    return 1;
  }

  @override
  set icon(Image? value) {}

  @override
  void setTooltip(String? tooltip) {}

  @override
  void setContextMenuTrigger(ContextMenuTrigger trigger) {}

  @override
  void setContextMenu(Menu? menu) {}

  @override
  bool setVisible(bool visible) => true;

  @override
  void dispose() => disposed = true;

  @override
  dynamic noSuchMethod(Invocation invocation) => null;
}

class FakeTrayMenu implements Menu {
  final items = <MenuItem?>[];
  bool separated = false;

  @override
  void addItem(MenuItem? item) => items.add(item);

  @override
  void addSeparator() => separated = true;

  @override
  dynamic noSuchMethod(Invocation invocation) => null;
}

class FakeTrayMenuItem implements MenuItem {
  final listeners = <FutureOr<void> Function(MenuEvent)>[];

  @override
  ListenerId addListener(FutureOr<void> Function(MenuEvent) callback) {
    listeners.add(callback);
    return 1;
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => null;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'desktop notifications initialize once and report both transitions',
    () async {
      expect(isDesktop, isTrue);
      final driver = FakeNotifications();
      final service = DesktopNotificationService.withDriverForTesting(driver);
      await service.initialize();
      await service.initialize();
      await service.requestPermission();
      await service.agentConnectionChanged(true);
      await service.agentConnectionChanged(false);
      expect(driver.initializations, 1);
      expect(driver.permissionRequests, 1);
      expect(driver.shown.map((item) => item.$2), [
        'RustFRP 已连接',
        'RustFRP 连接断开',
      ]);

      driver.failShow = true;
      await service.agentConnectionChanged(false);
    },
  );

  test(
    'native notification driver initializes and sends a notification',
    () async {
      expect(isDesktop, isTrue);
      final oldPlatform = debugDefaultTargetPlatformOverride;
      debugDefaultTargetPlatformOverride = TargetPlatform.android;
      AndroidFlutterLocalNotificationsPlugin.registerWith();
      const channel = MethodChannel(
        'dexterous.com/flutter/local_notifications',
      );
      final calls = <MethodCall>[];
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(channel, (call) async {
        calls.add(call);
        return true;
      });
      addTearDown(() {
        debugDefaultTargetPlatformOverride = oldPlatform;
        messenger.setMockMethodCallHandler(channel, null);
      });

      await DesktopNotificationService.instance.initialize();
      await DesktopNotificationService.instance.requestPermission();
      await DesktopNotificationService.instance.agentConnectionChanged(true);
      expect(
        calls.map((call) => call.method),
        containsAll(['initialize', 'show']),
      );
    },
  );

  test(
    'window setup and close-to-tray lifecycle are safe in desktop hosts',
    () async {
      const channel = MethodChannel('window_manager');
      final calls = <MethodCall>[];
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(channel, (call) async {
        calls.add(call);
        if ([
          'isFullScreen',
          'isMaximized',
          'isMinimized',
        ].contains(call.method)) {
          return false;
        }
        return null;
      });
      addTearDown(() {
        debugWindowShell = null;
        messenger.setMockMethodCallHandler(channel, null);
      });

      debugWindowShell = false;
      expect(windowShellActive, false);
      expect(hasCustomWindowControls, false);
      debugWindowShell = true;
      expect(windowShellActive, true);
      expect(hasCustomWindowControls, !Platform.isMacOS);

      final nativeBindings = DesktopTrayBindings.native();
      expect(nativeBindings.createTray, isA<Function>());
      expect(nativeBindings.loadImage, isA<Function>());
      expect(nativeBindings.createMenu, isA<Function>());
      expect(nativeBindings.createMenuItem, isA<Function>());

      await initDesktopWindow(showAtStartup: false);
      await initDesktopWindow(showAtStartup: true);
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(
        calls.where((call) => call.method == 'ensureInitialized'),
        hasLength(2),
      );
      expect(calls.any((call) => call.method == 'setTitleBarStyle'), true);
      expect(windowShellActive, true);

      SharedPreferences.setMockInitialValues({'close_to_tray': true});
      final preferences = await SharedPreferences.getInstance();
      final tray = FakeTray();
      final menu = FakeTrayMenu();
      final showItem = FakeTrayMenuItem();
      final exitItem = FakeTrayMenuItem();
      final lifecycle = DesktopAppLifecycle(
        preferences,
        trayBindings: DesktopTrayBindings(
          createTray: () => tray,
          loadImage: (_) => null,
          createMenu: () => menu,
          createMenuItem: (label) =>
              label == '显示 RustFRP' ? showItem : exitItem,
        ),
      );
      await lifecycle.initialize(startHidden: true);
      expect(menu.items, [showItem, exitItem]);
      expect(menu.separated, true);
      await tray.onEvent!(TrayIconClickedEvent(trayIconId: 1));
      await tray.onEvent!(TrayIconDoubleClickedEvent(trayIconId: 1));
      lifecycle.onWindowClose();
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(calls.any((call) => call.method == 'hide'), true);

      await preferences.setBool('close_to_tray', false);
      lifecycle.onWindowClose();
      await Future<void>.delayed(const Duration(milliseconds: 10));
      expect(calls.any((call) => call.method == 'close'), true);
      await showItem.listeners.single(MenuItemClickedEvent(itemId: 1));
      await exitItem.listeners.single(MenuItemClickedEvent(itemId: 2));

      final brokenTray = FakeTray();
      await DesktopAppLifecycle(
        preferences,
        trayBindings: DesktopTrayBindings(
          createTray: () => brokenTray,
          loadImage: (_) => null,
          createMenu: () => null,
          createMenuItem: (_) => null,
        ),
      ).initialize(startHidden: true);
      expect(brokenTray.disposed, true);
      expect(calls.where((call) => call.method == 'setPreventClose').length, 5);
    },
  );
}
