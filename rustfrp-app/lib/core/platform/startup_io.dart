import 'dart:convert';
import 'dart:io';

typedef StartupCommandRunner = Future<ProcessResult> Function(
  String executable,
  List<String> arguments,
);

String startupDefinitionPath({
  required String home,
  required bool windows,
  required bool macos,
}) {
  if (windows) return 'RustFRP-Startup';
  final sep = Platform.pathSeparator;
  if (macos) {
    return '$home$sep'
        'Library$sep'
        'LaunchAgents$sep'
        'com.rustfrp.desktop.plist';
  }
  return '$home$sep'
      '.config$sep'
      'autostart$sep'
      'rustfrp.desktop';
}

String _xml(String value) => const HtmlEscape().convert(value);

String _desktopExec(String value) =>
    '"${value.replaceAll(r'\', r'\\').replaceAll('"', r'\"').replaceAll('%', r'\%')}"';

String startupPlist(String appPath) =>
    '<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>Label</key><string>com.rustfrp.desktop</string><key>ProgramArguments</key><array><string>${_xml(appPath)}</string><string>--startup-to-tray</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>';

String startupDesktopEntry(String appPath) =>
    '[Desktop Entry]\nType=Application\nName=RustFRP\nExec=${_desktopExec(appPath)} --startup-to-tray\nTerminal=false\nX-GNOME-Autostart-enabled=true\n';

Future<ProcessResult> _run(String executable, List<String> arguments) =>
    Process.run(executable, arguments).timeout(const Duration(seconds: 5));

Future<String> _home({
  Map<String, String>? environment,
  required bool windows,
}) async {
  final env = environment ?? Platform.environment;
  final value = env[windows ? 'USERPROFILE' : 'HOME'];
  if (value == null || value.isEmpty) {
    throw const FileSystemException('无法确定当前用户目录');
  }
  return value;
}

Future<bool> startupEnabled({
  Map<String, String>? environment,
  bool? windows,
  bool? macos,
  StartupCommandRunner? run,
}) async {
  final isWindows = windows ?? Platform.isWindows;
  final isMacos = macos ?? Platform.isMacOS;
  final home = await _home(environment: environment, windows: isWindows);
  if (isWindows) {
    final result = await (run ?? _run)('schtasks.exe', [
      '/Query',
      '/TN',
      startupDefinitionPath(home: home, windows: true, macos: false),
    ]);
    return result.exitCode == 0;
  }
  return File(startupDefinitionPath(home: home, windows: false, macos: isMacos))
      .exists();
}

Future<void> setStartupEnabled(
  bool enabled, {
  String? appPath,
  Map<String, String>? environment,
  bool? windows,
  bool? macos,
  StartupCommandRunner? run,
}) async {
  final isWindows = windows ?? Platform.isWindows;
  final isMacos = macos ?? Platform.isMacOS;
  final home = await _home(environment: environment, windows: isWindows);
  final binary = appPath ?? Platform.resolvedExecutable;
  if (binary.isEmpty || !File(binary).isAbsolute) {
    throw const FileSystemException('应用路径必须是绝对路径');
  }
  final invoke = run ?? _run;
  if (isWindows) {
    final name = startupDefinitionPath(home: home, windows: true, macos: false);
    final result = enabled
        ? await invoke('schtasks.exe', [
            '/Create',
            '/SC',
            'ONLOGON',
            '/TN',
            name,
            '/TR',
            '"$binary" --startup-to-tray',
            '/F',
          ])
        : await invoke('schtasks.exe', ['/Delete', '/TN', name, '/F']);
    if (enabled && result.exitCode != 0) {
      throw FileSystemException('Windows 开机启动设置失败: ${result.stderr}');
    }
    return;
  }
  final path = startupDefinitionPath(
    home: home,
    windows: false,
    macos: isMacos,
  );
  final file = File(path);
  if (!enabled) {
    if (isMacos) {
      final uid = await invoke('/usr/bin/id', ['-u']);
      if (uid.exitCode == 0) {
        await invoke('/bin/launchctl', [
          'bootout',
          'gui/${uid.stdout.toString().trim()}/com.rustfrp.desktop',
        ]);
      }
    }
    if (await file.exists()) await file.delete();
    return;
  }
  await file.parent.create(recursive: true);
  final contents = isMacos ? startupPlist(binary) : startupDesktopEntry(binary);
  await file.writeAsString(contents, flush: true);
  final chmod = await invoke('/bin/chmod', ['600', file.path]);
  if (chmod.exitCode != 0) {
    await file.delete();
    throw const FileSystemException('无法设置开机启动文件权限');
  }
}
