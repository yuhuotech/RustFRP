import 'dart:io';

import 'package:flutter/foundation.dart';

typedef InstallCommand = Future<ProcessResult> Function(
  String executable,
  List<String> arguments,
);
typedef InstallHandoff = Future<void> Function(String script);
typedef WindowsInstaller = Future<void> Function(File package);
typedef DetachedProcessStart = Future<int> Function(
  String executable,
  List<String> arguments,
);

/// OS facts and process hooks make each package strategy testable without replacing the host app.
class AppUpdateEnvironment {
  const AppUpdateEnvironment({
    required this.isWeb,
    required this.os,
    required this.architecture,
    required this.executable,
    required this.debianInstalled,
    required this.windowsInstalled,
    required this.isWritable,
    required this.run,
    required this.handoff,
    required this.installWindows,
  });

  final bool isWeb, debianInstalled, windowsInstalled;
  final String os, architecture, executable;
  final bool Function(String path) isWritable;
  final InstallCommand run;
  final InstallHandoff handoff;
  final WindowsInstaller installWindows;

  @visibleForTesting
  static bool windowsInstallerRegistered(
    String executable, {
    required bool isWindows,
    bool Function(String path)? fileExists,
  }) =>
      isWindows &&
      (fileExists ?? _fileExists)(
        '${File(executable).parent.path}\\unins000.exe',
      );

  static bool _fileExists(String path) => File(path).existsSync();

  static Future<AppUpdateEnvironment> current({bool? web}) async {
    if (web ?? kIsWeb) {
      return AppUpdateEnvironment(
        isWeb: true,
        os: 'web',
        architecture: '',
        executable: '',
        debianInstalled: false,
        windowsInstalled: false,
        isWritable: (_) => false,
        run: Process.run,
        handoff: (_) async {},
        installWindows: (_) async {},
      );
    }
    final os = AppInstallPlan.detectOperatingSystem(
      macOS: Platform.isMacOS,
      windows: Platform.isWindows,
      linux: Platform.isLinux,
    );
    final executable = Platform.resolvedExecutable;
    final arch = await AppInstallPlan._architecture();
    return AppUpdateEnvironment(
      isWeb: kIsWeb,
      os: os,
      architecture: arch,
      executable: executable,
      debianInstalled:
          os == 'linux' && executable.startsWith('/usr/lib/rustfrp/app/'),
      windowsInstalled: windowsInstallerRegistered(
        executable,
        isWindows: Platform.isWindows,
      ),
      isWritable: (path) => AppInstallPlan._writable(Directory(path)),
      run: Process.run,
      handoff: AppInstallPlan.runAfterExit,
      installWindows: AppInstallPlan.installWindows,
    );
  }
}

/// How this desktop installation can apply a signed release package.
class AppInstallPlan {
  AppInstallPlan._(this.assetName, this.manualReason, this._install);

  final String? assetName;
  final String? manualReason;
  final Future<void> Function(File package)? _install;

  bool get canInstall => assetName != null && _install != null;

  Future<void> install(File package) async => _install!(package);

  @visibleForTesting
  factory AppInstallPlan.fake(
    String assetName, {
    Future<void> Function(File package)? install,
  }) => AppInstallPlan._(assetName, null, install ?? (_) async {});

  @visibleForTesting
  factory AppInstallPlan.manual(String reason) =>
      AppInstallPlan._(null, reason, null);

  static Future<AppInstallPlan> forRelease(
    List<String> assets, {
    AppUpdateEnvironment? environment,
    Future<AppUpdateEnvironment> Function()? loadEnvironment,
  }) async {
    late final AppUpdateEnvironment env;
    try {
      env =
          environment ??
          await (loadEnvironment ?? AppUpdateEnvironment.current)();
    } on Object catch (_) {
      return AppInstallPlan.manual('无法识别当前安装方式，请前往 Release 页面手动更新');
    }
    if (env.isWeb) return AppInstallPlan.manual('Web 面板随 Controller 更新');
    try {
      if (env.os == 'macos') return await _mac(assets, env);
      if (env.os == 'windows') return _windows(assets, env);
      if (env.os == 'linux') return await _linux(assets, env);
    } on Object catch (_) {
      return AppInstallPlan.manual('无法识别当前安装方式，请前往 Release 页面手动更新');
    }
    return AppInstallPlan.manual('当前平台暂不支持自动更新，请前往下载页');
  }

  static String? _find(List<String> assets, String suffix) =>
      assets.where((name) => name.endsWith(suffix)).firstOrNull;

  static Future<AppInstallPlan> _mac(
    List<String> assets,
    AppUpdateEnvironment env,
  ) async {
    final asset = switch (env.architecture) {
      'arm64' => _find(assets, 'rustfrp-macos-arm64.zip'),
      'x86_64' => _find(assets, 'rustfrp-macos-x86_64.zip'),
      _ => null,
    };
    if (asset == null) return AppInstallPlan.manual('此版本没有本机架构的 macOS 更新包');
    final executable = env.executable;
    final marker = '.app/Contents/MacOS/';
    final offset = executable.indexOf(marker);
    if (offset < 0) return AppInstallPlan.manual('无法定位 RustFRP.app');
    final bundle = executable.substring(0, offset + 4);
    if (bundle.startsWith('/Volumes/') ||
        bundle.contains('/AppTranslocation/')) {
      return AppInstallPlan.manual('请先把 RustFRP.app 移到「应用程序」文件夹再更新');
    }
    final parent = Directory(File(bundle).parent.path);
    if (!env.isWritable(parent.path)) {
      return AppInstallPlan.manual('应用程序目录不可写，请手动安装更新');
    }
    return AppInstallPlan._(
      asset,
      null,
      (pkg) => _installMac(pkg, bundle, env),
    );
  }

  static Future<void> _installMac(
    File zip,
    String bundle,
    AppUpdateEnvironment env,
  ) async {
    final parent = Directory(File(bundle).parent.path);
    final stage = await parent.createTemp('.rustfrp-update-');
    final result = await env.run('/usr/bin/ditto', [
      '-x',
      '-k',
      zip.path,
      stage.path,
    ]);
    if (result.exitCode != 0) throw '解压 macOS 更新包失败';
    final fresh = stage
        .listSync()
        .whereType<Directory>()
        .where((dir) => dir.path.endsWith('.app'))
        .firstOrNull;
    if (fresh == null) throw '更新包里没有 .app';
    await env.handoff('''
BUNDLE=${_quote(bundle)}
NEW=${_quote(fresh.path)}
OLD="\$BUNDLE.old.\$\$"
mv "\$BUNDLE" "\$OLD" || exit 1
if mv "\$NEW" "\$BUNDLE"; then
  /usr/bin/open "\$BUNDLE"
  sleep 3
  if pgrep -f "\$BUNDLE/Contents/MacOS/rustfrp_app" >/dev/null; then
    rm -rf "\$OLD" "${_quote(stage.path)}" "${_quote(zip.parent.path)}"
    exit 0
  fi
  rm -rf "\$BUNDLE"
fi
mv "\$OLD" "\$BUNDLE"
''');
  }

  static AppInstallPlan _windows(
    List<String> assets,
    AppUpdateEnvironment env,
  ) {
    const asset = 'rustfrp-windows-x86_64-setup.exe';
    if (!assets.contains(asset)) {
      return AppInstallPlan.manual('此版本没有 Windows x64 安装程序');
    }
    if (!env.windowsInstalled) {
      return AppInstallPlan.manual('当前是免安装目录，自动更新仅支持安装程序版本');
    }
    return AppInstallPlan._(asset, null, env.installWindows);
  }

  static Future<AppInstallPlan> _linux(
    List<String> assets,
    AppUpdateEnvironment env,
  ) async {
    if (env.architecture != 'x86_64') {
      return AppInstallPlan.manual('当前只发布 Linux x86_64 更新包');
    }
    final executable = env.executable;
    final deb = _find(assets, 'rustfrp-linux-x86_64.deb');
    if (env.debianInstalled && deb != null) {
      return AppInstallPlan._(
        deb,
        null,
        (pkg) => _installDeb(pkg, executable, env),
      );
    }
    final tar = _find(assets, 'rustfrp-linux-x86_64.tar.gz');
    final appDir = Directory(File(executable).parent.path);
    final root = Directory(appDir.parent.path);
    if (tar != null &&
        appDir.path.endsWith('/app') &&
        env.isWritable(root.parent.path)) {
      return AppInstallPlan._(
        tar,
        null,
        (pkg) => _installTar(pkg, root, executable, env),
      );
    }
    return AppInstallPlan.manual('此安装位置不支持自动替换，请下载安装包手动更新');
  }

  static Future<void> installWindows(
    File pkg, {
    DetachedProcessStart? start,
    void Function()? quit,
  }) async {
    final launch = start ?? _startDetached;
    final installerPid = await launch(pkg.path, [
      '/VERYSILENT',
      '/SUPPRESSMSGBOXES',
      '/NORESTART',
      '/CLOSEAPPLICATIONS',
      '/FORCECLOSEAPPLICATIONS',
    ]);
    final tempDir = pkg.parent.path.replaceAll("'", "''");
    await launch('powershell.exe', [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      "Wait-Process -Id $installerPid; Remove-Item -LiteralPath '$tempDir' -Recurse -Force",
    ]);
    (quit ?? (() => exit(0)))();
  }

  static Future<void> _installDeb(
    File pkg,
    String executable,
    AppUpdateEnvironment env,
  ) async {
    await env.handoff('''
PKG=${_quote(pkg.path)}
APP=${_quote(executable)}
TMP=${_quote(pkg.parent.path)}
export DEBIAN_FRONTEND=noninteractive
if [ "\$(id -u)" = 0 ]; then dpkg -i "\$PKG"
elif sudo -n true 2>/dev/null; then sudo -n dpkg -i "\$PKG"
else pkexec dpkg -i "\$PKG"; fi
RESULT=\$?
rm -rf "\$TMP"
if [ \$RESULT -eq 0 ]; then setsid "\$APP" >/dev/null 2>&1 & fi
''');
  }

  static Future<void> _installTar(
    File pkg,
    Directory root,
    String executable,
    AppUpdateEnvironment env,
  ) async {
    final parent = Directory(root.parent.path);
    final stage = await parent.createTemp('.rustfrp-update-');
    final extract = await env.run('/bin/tar', [
      '-xzf',
      pkg.path,
      '-C',
      stage.path,
    ]);
    if (extract.exitCode != 0) throw '解压 Linux 更新包失败';
    final fresh = Directory('${stage.path}/rustfrp-linux-x86_64');
    if (!await fresh.exists()) throw '更新包目录结构无效';
    final newExecutable =
        '${root.path}/app/${File(executable).uri.pathSegments.last}';
    await env.handoff('''
ROOT=${_quote(root.path)}
NEW=${_quote(fresh.path)}
APP=${_quote(newExecutable)}
OLD="\$ROOT.old.\$\$"
mv "\$ROOT" "\$OLD" || exit 1
if mv "\$NEW" "\$ROOT"; then
  setsid "\$APP" >/dev/null 2>&1 &
  sleep 3
  if pgrep -f "\$APP" >/dev/null; then rm -rf "\$OLD" "${_quote(stage.path)}" "${_quote(pkg.parent.path)}"; exit 0; fi
  rm -rf "\$ROOT"
fi
mv "\$OLD" "\$ROOT"
''');
  }

  static Future<String> _architecture() async {
    final result = await Process.run(
      Platform.isWindows ? 'cmd.exe' : '/usr/bin/uname',
      Platform.isWindows ? ['/c', 'echo', '%PROCESSOR_ARCHITECTURE%'] : ['-m'],
    );
    if (result.exitCode != 0) return '';
    return result.stdout.toString().trim().toLowerCase();
  }

  static bool _writable(Directory directory) {
    try {
      final file = File('${directory.path}/.rustfrp-update-$pid');
      file.writeAsStringSync('check');
      file.deleteSync();
      return true;
    } on Object {
      return false;
    }
  }

  static String detectOperatingSystem({
    required bool macOS,
    required bool windows,
    required bool linux,
  }) => macOS
      ? 'macos'
      : windows
      ? 'windows'
      : linux
      ? 'linux'
      : 'other';

  static String _quote(String value) => "'${value.replaceAll("'", "'\\''")}'";

  static Future<void> runAfterExit(
    String body, {
    InstallHandoff? launch,
    void Function()? quit,
  }) async {
    final dir = await Directory.systemTemp.createTemp('rustfrp-update-');
    final script = File('${dir.path}/install.sh')
      ..writeAsStringSync('''#!/bin/sh
trap "rm -rf ${_quote(dir.path)}" EXIT
i=0
while kill -0 $pid 2>/dev/null && [ \$i -lt 100 ]; do sleep 0.3; i=\$((i+1)); done
$body
''');
    if (launch == null) {
      await Process.start('/bin/sh', [
        script.path,
      ], mode: ProcessStartMode.detached);
    } else {
      await launch(script.path);
    }
    (quit ?? (() => exit(0)))();
  }

  static Future<int> _startDetached(
    String executable,
    List<String> arguments,
  ) async => (await Process.start(
    executable,
    arguments,
    mode: ProcessStartMode.detached,
  )).pid;
}
