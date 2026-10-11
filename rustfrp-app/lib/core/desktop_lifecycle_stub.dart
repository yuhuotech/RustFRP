import 'package:shared_preferences/shared_preferences.dart';

class DesktopAppLifecycle {
  DesktopAppLifecycle(SharedPreferences preferences);
  Future<void> initialize({required bool startHidden}) async {}
}
