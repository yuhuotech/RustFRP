class DesktopNotificationService {
  static final DesktopNotificationService instance =
      DesktopNotificationService._();
  DesktopNotificationService._();
  Future<void> initialize() async {}
  Future<void> requestPermission() async {}
  Future<void> agentConnectionChanged(bool connected) async {}
}
