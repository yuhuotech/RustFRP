Future<bool> startupEnabled() async => false;

Future<void> setStartupEnabled(bool enabled) async {
  throw UnsupportedError('开机启动仅支持桌面端');
}
