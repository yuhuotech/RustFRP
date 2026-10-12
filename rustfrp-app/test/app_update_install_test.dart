import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/app_update_install.dart';

void main() {
  AppUpdateEnvironment machine({
    required String os,
    bool isWeb = false,
    String architecture = 'x86_64',
    String executable = '/tmp/rustfrp_app',
    bool debianInstalled = false,
    bool windowsInstalled = false,
    bool writable = true,
    bool Function(String path)? canWrite,
    Future<ProcessResult> Function(String, List<String>)? run,
    Future<void> Function(String)? handoff,
    Future<void> Function(File)? installWindows,
  }) => AppUpdateEnvironment(
    isWeb: isWeb,
    os: os,
    architecture: architecture,
    executable: executable,
    debianInstalled: debianInstalled,
    windowsInstalled: windowsInstalled,
    isWritable: canWrite ?? (_) => writable,
    run: run ?? (_, _) async => ProcessResult(1, 0, '', ''),
    handoff: handoff ?? (_) async {},
    installWindows: installWindows ?? (_) async {},
  );

  test(
    'macOS plan selects architecture, locates the bundle and stages atomically',
    () async {
      final root = await Directory.systemTemp.createTemp(
        'rustfrp-mac-update-test-',
      );
      addTearDown(() => root.delete(recursive: true));
      final bundle = Directory('${root.path}/RustFRP.app')
        ..createSync(recursive: true);
      final executable = '${bundle.path}/Contents/MacOS/rustfrp_app';
      final commands = <List<String>>[];
      final scripts = <String>[];
      final plan = await AppInstallPlan.forRelease(
        const ['rustfrp-macos-arm64.zip'],
        environment: machine(
          os: 'macos',
          architecture: 'arm64',
          executable: executable,
          run: (command, args) async {
            commands.add([command, ...args]);
            await Directory('${args.last}/rustfrp_app.app').create();
            return ProcessResult(1, 0, '', '');
          },
          handoff: (script) async => scripts.add(script),
        ),
      );
      expect(plan.assetName, 'rustfrp-macos-arm64.zip');
      final zip = File('${root.path}/update.zip')..writeAsStringSync('zip');
      await plan.install(zip);
      expect(commands.single.first, '/usr/bin/ditto');
      expect(scripts.single, contains('mv "\$BUNDLE" "\$OLD"'));
      expect(scripts.single, contains('mv "\$OLD" "\$BUNDLE"'));

      final x64 = await AppInstallPlan.forRelease(
        const ['rustfrp-macos-x86_64.zip'],
        environment: machine(
          os: 'macos',
          architecture: 'arm64',
          executable: executable,
        ),
      );
      expect(x64.canInstall, isFalse);
      expect(x64.manualReason, contains('架构'));
      final intel = await AppInstallPlan.forRelease(
        const ['rustfrp-macos-x86_64.zip'],
        environment: machine(
          os: 'macos',
          architecture: 'x86_64',
          executable: '/tmp/rustfrp_app',
        ),
      );
      expect(intel.manualReason, contains('无法定位'));
    },
  );

  test(
    'macOS refuses translocated, volume-mounted and unwritable bundles',
    () async {
      for (final path in [
        '/Volumes/RustFRP/RustFRP.app/Contents/MacOS/rustfrp_app',
        '/Applications/AppTranslocation/RustFRP.app/Contents/MacOS/rustfrp_app',
      ]) {
        final plan = await AppInstallPlan.forRelease(
          const ['rustfrp-macos-arm64.zip'],
          environment: machine(
            os: 'macos',
            architecture: 'arm64',
            executable: path,
          ),
        );
        expect(plan.canInstall, isFalse);
        expect(plan.manualReason, contains('移到'));
      }
      final blocked = await AppInstallPlan.forRelease(
        const ['rustfrp-macos-arm64.zip'],
        environment: machine(
          os: 'macos',
          architecture: 'arm64',
          executable: '/Applications/RustFRP.app/Contents/MacOS/rustfrp_app',
          writable: false,
        ),
      );
      expect(blocked.manualReason, contains('不可写'));
    },
  );

  test('Windows installs only over a registered setup installation', () async {
    File? received;
    final plan = await AppInstallPlan.forRelease(
      const ['rustfrp-windows-x86_64-setup.exe'],
      environment: machine(
        os: 'windows',
        windowsInstalled: true,
        installWindows: (file) async => received = file,
      ),
    );
    expect(plan.assetName, 'rustfrp-windows-x86_64-setup.exe');
    final setup = File('${Directory.systemTemp.path}/setup.exe')
      ..writeAsStringSync('installer');
    await plan.install(setup);
    expect(received?.path, setup.path);

    final portable = await AppInstallPlan.forRelease(const [
      'rustfrp-windows-x86_64-setup.exe',
    ], environment: machine(os: 'windows'));
    expect(portable.manualReason, contains('免安装'));
    final noSetup = await AppInstallPlan.forRelease(
      const [],
      environment: machine(os: 'windows', windowsInstalled: true),
    );
    expect(noSetup.manualReason, contains('没有 Windows'));
  });

  test(
    'Linux selects deb or a writable tar bundle and creates a rollback handoff',
    () async {
      final debScripts = <String>[];
      final deb = await AppInstallPlan.forRelease(
        const ['rustfrp-linux-x86_64.deb'],
        environment: machine(
          os: 'linux',
          debianInstalled: true,
          executable: '/usr/lib/rustfrp/app/rustfrp_app',
          handoff: (script) async => debScripts.add(script),
        ),
      );
      expect(deb.assetName, 'rustfrp-linux-x86_64.deb');
      final package = File('${Directory.systemTemp.path}/rustfrp.deb')
        ..writeAsStringSync('deb');
      await deb.install(package);
      expect(debScripts.single, contains('pkexec dpkg -i'));

      final root = await Directory.systemTemp.createTemp(
        'rustfrp-tar-update-test-',
      );
      addTearDown(() => root.delete(recursive: true));
      final appRoot = Directory('${root.path}/rustfrp-linux-x86_64');
      final scripts = <String>[];
      final tar = await AppInstallPlan.forRelease(
        const ['rustfrp-linux-x86_64.tar.gz'],
        environment: machine(
          os: 'linux',
          executable: '${appRoot.path}/app/rustfrp_app',
          run: (command, args) async {
            expect(command, '/bin/tar');
            await Directory('${args[3]}/rustfrp-linux-x86_64').create();
            return ProcessResult(1, 0, '', '');
          },
          handoff: (script) async => scripts.add(script),
        ),
      );
      expect(tar.assetName, 'rustfrp-linux-x86_64.tar.gz');
      final archive = File('${root.path}/update.tar.gz')
        ..writeAsStringSync('archive');
      await tar.install(archive);
      expect(scripts.single, contains('mv "\$ROOT" "\$OLD"'));

      final locked = await AppInstallPlan.forRelease(
        const ['rustfrp-linux-x86_64.tar.gz'],
        environment: machine(
          os: 'linux',
          executable: '${appRoot.path}/app/rustfrp_app',
          writable: false,
        ),
      );
      expect(locked.canInstall, isFalse);

      final planFailure = await AppInstallPlan.forRelease(
        const ['rustfrp-linux-x86_64.tar.gz'],
        environment: machine(
          os: 'linux',
          executable: '${appRoot.path}/app/rustfrp_app',
          canWrite: (_) => throw StateError('permission probe failed'),
        ),
      );
      expect(planFailure.canInstall, isFalse);
      expect(planFailure.manualReason, contains('无法识别'));

      final wrongArchitecture = await AppInstallPlan.forRelease(const [
        'rustfrp-linux-x86_64.deb',
      ], environment: machine(os: 'linux', architecture: 'aarch64'));
      expect(wrongArchitecture.manualReason, contains('x86_64'));

      final extractionFailed = await AppInstallPlan.forRelease(
        const ['rustfrp-linux-x86_64.tar.gz'],
        environment: machine(
          os: 'linux',
          executable: '${appRoot.path}/app/rustfrp_app',
          run: (_, _) async => ProcessResult(1, 1, '', 'tar failed'),
        ),
      );
      await expectLater(
        extractionFailed.install(archive),
        throwsA(contains('解压')),
      );
      final missingRoot = await AppInstallPlan.forRelease(
        const ['rustfrp-linux-x86_64.tar.gz'],
        environment: machine(
          os: 'linux',
          executable: '${appRoot.path}/app/rustfrp_app',
        ),
      );
      await expectLater(
        missingRoot.install(archive),
        throwsA(contains('目录结构')),
      );
    },
  );

  test(
    'unsupported platform and package list fail to manual instructions',
    () async {
      final unsupported = await AppInstallPlan.forRelease(
        const [],
        environment: machine(os: 'other'),
      );
      expect(unsupported.canInstall, isFalse);
      expect(unsupported.manualReason, contains('不支持'));
      final current = await AppUpdateEnvironment.current();
      final temp = await Directory.systemTemp.createTemp(
        'rustfrp-writable-test-',
      );
      addTearDown(() => temp.delete(recursive: true));
      expect(current.isWritable(temp.path), isTrue);
      expect(current.isWritable('${temp.path}/missing'), isFalse);
      final webEnvironment = await AppUpdateEnvironment.current(web: true);
      expect(webEnvironment.isWeb, isTrue);
      expect(webEnvironment.isWritable('/'), isFalse);
      await webEnvironment.handoff('ignored');
      await webEnvironment.installWindows(File('ignored.exe'));
      final web = await AppInstallPlan.forRelease(const [
        'rustfrp-macos-arm64.zip',
      ], environment: machine(os: 'web', isWeb: true));
      expect(web.manualReason, contains('Web 面板'));
      final uninstaller = File('${temp.path}\\unins000.exe')
        ..writeAsStringSync('installed');
      addTearDown(() => uninstaller.delete());
      expect(
        AppUpdateEnvironment.windowsInstallerRegistered(
          '${temp.path}/app.exe',
          isWindows: true,
        ),
        isTrue,
      );
      expect(
        AppUpdateEnvironment.windowsInstallerRegistered(
          '/Applications/RustFRP/app.exe',
          isWindows: false,
        ),
        isFalse,
      );
      final environmentFailure = await AppInstallPlan.forRelease(
        const [],
        loadEnvironment: () async => throw StateError('platform probe failed'),
      );
      expect(environmentFailure.canInstall, isFalse);
      expect(environmentFailure.manualReason, contains('无法识别'));
      expect(
        AppInstallPlan.detectOperatingSystem(
          macOS: true,
          windows: true,
          linux: true,
        ),
        'macos',
      );
      expect(
        AppInstallPlan.detectOperatingSystem(
          macOS: false,
          windows: true,
          linux: true,
        ),
        'windows',
      );
      expect(
        AppInstallPlan.detectOperatingSystem(
          macOS: false,
          windows: false,
          linux: true,
        ),
        'linux',
      );
    },
  );

  test('after-exit and Windows installer scripts can be verified without replacing the app', () async {
    var handoffText = '';
    var quit = false;
    await AppInstallPlan.runAfterExit(
      'echo update',
      launch: (path) async => handoffText = File(path).readAsStringSync(),
      quit: () => quit = true,
    );
    expect(handoffText, contains('kill -0 $pid'));
    expect(handoffText, contains('echo update'));
    expect(quit, isTrue);

    final calls = <(String, List<String>)>[];
    quit = false;
    await AppInstallPlan.installWindows(
      File("${Directory.systemTemp.path}/quoted' dir/setup.exe"),
      start: (executable, arguments) async {
        calls.add((executable, arguments));
        return 42;
      },
      quit: () => quit = true,
    );
    expect(calls, hasLength(2));
    expect(calls.first.$2, contains('/FORCECLOSEAPPLICATIONS'));
    expect(calls.last.$1, 'powershell.exe');
    expect(calls.last.$2.last, contains("quoted'' dir"));
    expect(quit, isTrue);
    await AppInstallPlan.runAfterExit('true', quit: () {});
    if (!Platform.isWindows) {
      await expectLater(
        AppInstallPlan.installWindows(
          File(Platform.resolvedExecutable),
          quit: () {},
        ),
        throwsA(isA<ProcessException>()),
      );
    }
    await expectLater(
      AppInstallPlan.installWindows(
        File('${Directory.systemTemp.path}/rustfrp-missing-$pid.exe'),
        quit: () {},
      ),
      throwsA(isA<ProcessException>()),
    );
  });

  test(
    'archive extraction rejects failures and packages without an app bundle',
    () async {
      final root = await Directory.systemTemp.createTemp(
        'rustfrp-extract-test-',
      );
      addTearDown(() => root.delete(recursive: true));
      final executable = '${root.path}/RustFRP.app/Contents/MacOS/rustfrp_app';
      final failedMac = await AppInstallPlan.forRelease(
        const ['rustfrp-macos-arm64.zip'],
        environment: machine(
          os: 'macos',
          architecture: 'arm64',
          executable: executable,
          run: (_, _) async => ProcessResult(1, 1, '', 'extract failed'),
        ),
      );
      await expectLater(
        failedMac.install(File('${root.path}/bad.zip')),
        throwsA(contains('解压')),
      );
      final missingApp = await AppInstallPlan.forRelease(
        const ['rustfrp-macos-arm64.zip'],
        environment: machine(
          os: 'macos',
          architecture: 'arm64',
          executable: executable,
          run: (_, _) async => ProcessResult(1, 0, '', ''),
        ),
      );
      await expectLater(
        missingApp.install(File('${root.path}/empty.zip')),
        throwsA(contains('没有 .app')),
      );
    },
  );
}
