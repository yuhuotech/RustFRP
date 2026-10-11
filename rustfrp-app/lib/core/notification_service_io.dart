import 'dart:io';

import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:flutter_local_notifications/flutter_local_notifications.dart';

import 'desktop.dart';

abstract interface class DesktopNotificationDriver {
  Future<void> initialize(InitializationSettings settings);
  Future<void> requestPermission();
  Future<void> show({
    required int id,
    required String title,
    required String body,
    required NotificationDetails details,
  });
}

class _FlutterNotificationDriver implements DesktopNotificationDriver {
  final FlutterLocalNotificationsPlugin _plugin =
      FlutterLocalNotificationsPlugin();

  @override
  Future<void> initialize(InitializationSettings settings) async {
    await _plugin.initialize(settings: settings);
  }

  @override
  Future<void> requestPermission() async {
    if (Platform.isMacOS) {
      await _plugin
          .resolvePlatformSpecificImplementation<
            MacOSFlutterLocalNotificationsPlugin
          >()
          ?.requestPermissions(alert: true, sound: true, badge: false);
    }
  }

  @override
  Future<void> show({
    required int id,
    required String title,
    required String body,
    required NotificationDetails details,
  }) => _plugin.show(
    id: id,
    title: title,
    body: body,
    notificationDetails: details,
  );
}

class DesktopNotificationService {
  DesktopNotificationService._(this._driver);
  static final DesktopNotificationService instance =
      DesktopNotificationService._(_FlutterNotificationDriver());
  @visibleForTesting
  static DesktopNotificationService withDriverForTesting(
    DesktopNotificationDriver driver,
  ) => DesktopNotificationService._(driver);

  final DesktopNotificationDriver _driver;
  bool _initialized = false;

  Future<void> initialize() async {
    if (!isDesktop || _initialized) return;
    await _driver.initialize(
      InitializationSettings(
        android: const AndroidInitializationSettings('ic_launcher'),
        macOS: const DarwinInitializationSettings(
          requestAlertPermission: false,
          requestBadgePermission: false,
          requestSoundPermission: false,
        ),
        linux: const LinuxInitializationSettings(
          defaultActionName: '打开 RustFRP',
        ),
        windows: const WindowsInitializationSettings(
          appName: 'RustFRP',
          appUserModelId: 'com.yuhuotech.rustfrp',
          guid: 'f04b1184-00d2-4537-a385-c3a4f10569c1',
        ),
      ),
    );
    _initialized = true;
  }

  Future<void> requestPermission() async {
    if (!isDesktop) return;
    await initialize();
    await _driver.requestPermission();
  }

  Future<void> agentConnectionChanged(bool connected) async {
    if (!_initialized) return;
    try {
      await _driver.show(
        id: 1,
        title: connected ? 'RustFRP 已连接' : 'RustFRP 连接断开',
        body: connected ? '本机隧道已恢复。' : '本机 Agent 与节点失去连接。',
        details: const NotificationDetails(
          macOS: DarwinNotificationDetails(),
          linux: LinuxNotificationDetails(),
          windows: WindowsNotificationDetails(),
        ),
      );
    } on Object {
      // Notification services can be unavailable or denied by the OS.
    }
  }
}
