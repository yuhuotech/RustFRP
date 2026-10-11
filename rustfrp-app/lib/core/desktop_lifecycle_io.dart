import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:tray_manager/tray_manager.dart';
import 'package:window_manager/window_manager.dart';

import 'desktop.dart';

class DesktopTrayBindings {
  const DesktopTrayBindings({
    required this.createTray,
    required this.loadImage,
    required this.createMenu,
    required this.createMenuItem,
  });

  factory DesktopTrayBindings.native() => DesktopTrayBindings(
    createTray: TrayIcon.create,
    loadImage: ImageAsset.fromAsset,
    createMenu: Menu.create,
    createMenuItem: (label) =>
        MenuItem.createWithLabelAndType(label, MenuItemType.normal),
  );

  final TrayIcon? Function() createTray;
  final Image? Function(String) loadImage;
  final Menu? Function() createMenu;
  final MenuItem? Function(String) createMenuItem;
}

class DesktopAppLifecycle with WindowListener {
  DesktopAppLifecycle(this._preferences, {DesktopTrayBindings? trayBindings})
    : _trayBindings = trayBindings ?? DesktopTrayBindings.native();
  final SharedPreferences _preferences;
  final DesktopTrayBindings _trayBindings;
  TrayIcon? _tray;
  bool _allowExit = false;

  Future<void> initialize({required bool startHidden}) async {
    if (!isDesktop) return;
    try {
      await windowManager.setPreventClose(true);
      windowManager.addListener(this);
      final tray = _trayBindings.createTray();
      if (tray == null) throw StateError('system tray is unavailable');
      _tray = tray;
      tray.icon = _trayBindings.loadImage('assets/brand/rustfrp-logo-128.png');
      tray.setTooltip('RustFRP');
      tray.setContextMenuTrigger(ContextMenuTrigger.rightClicked);
      tray.addListener((event) {
        if (event is TrayIconClickedEvent ||
            event is TrayIconDoubleClickedEvent) {
          unawaited(_showWindow());
        }
      });
      final menu = _trayBindings.createMenu();
      if (menu == null) throw StateError('tray menu is unavailable');
      final showItem = _trayBindings.createMenuItem('显示 RustFRP');
      final exitItem = _trayBindings.createMenuItem('退出应用');
      if (showItem == null || exitItem == null) {
        throw StateError('tray menu items are unavailable');
      }
      showItem.addListener((event) {
        if (event is MenuItemClickedEvent) unawaited(_showWindow());
      });
      exitItem.addListener((event) {
        if (event is MenuItemClickedEvent) unawaited(_exit());
      });
      menu.addItem(showItem);
      menu.addSeparator();
      menu.addItem(exitItem);
      tray.setContextMenu(menu);
      tray.setVisible(true);
      if (startHidden) await windowManager.hide();
    } on Object catch (error) {
      debugPrint('RustFRP tray setup failed: $error');
      _tray?.dispose();
      _tray = null;
      await windowManager.setPreventClose(false);
      if (startHidden) {
        await windowManager.show();
        await windowManager.focus();
      }
    }
  }

  Future<void> _showWindow() async {
    if (await windowManager.isMinimized()) await windowManager.restore();
    await windowManager.show();
    await windowManager.focus();
  }

  Future<void> _exit() async {
    _allowExit = true;
    await windowManager.setPreventClose(false);
    await windowManager.close();
  }

  @override
  void onWindowClose() async {
    if (_allowExit) return;
    if (_preferences.getBool('close_to_tray') ?? true) {
      await windowManager.hide();
      return;
    }
    await _exit();
  }
}
