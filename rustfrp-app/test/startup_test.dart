import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/platform/startup_io.dart';

void main() {
  test('startup files use token-free, shell-free app paths', () {
    final plist = startupPlist(
      '/Applications/Rust & FRP.app/Contents/MacOS/app',
    );
    expect(
      plist,
      contains(
        '<string>&#47;Applications&#47;Rust &amp; FRP.app&#47;Contents&#47;MacOS&#47;app</string>',
      ),
    );
    expect(plist, contains('<string>--startup-to-tray</string>'));
    final desktop = startupDesktopEntry('/opt/Rust FRP/rustfrp_app');
    expect(
      desktop,
      contains('Exec="/opt/Rust FRP/rustfrp_app" --startup-to-tray'),
    );
    expect(
      startupDefinitionPath(home: '/home/user', windows: false, macos: false),
      '/home/user/.config/autostart/rustfrp.desktop',
    );
    expect(
      startupDefinitionPath(home: r'C:\Users\me', windows: true, macos: false),
      'RustFRP-Startup',
    );
  });

  test(
    'Linux autostart enable and disable update only the current user file',
    () async {
      final home = await Directory.systemTemp.createTemp('rustfrp-startup-');
      addTearDown(() => home.delete(recursive: true));
      final path = startupDefinitionPath(
        home: home.path,
        windows: false,
        macos: false,
      );
      final app = '${home.path}/Rust FRP/rustfrp_app';
      await setStartupEnabled(
        true,
        appPath: app,
        environment: {'HOME': home.path},
        windows: false,
        macos: false,
      );
      expect(
        await startupEnabled(
          environment: {'HOME': home.path},
          windows: false,
          macos: false,
        ),
        true,
      );
      final contents = await File(path).readAsString();
      expect(contents, contains('"$app" --startup-to-tray'));
      await setStartupEnabled(
        false,
        appPath: app,
        environment: {'HOME': home.path},
        windows: false,
        macos: false,
      );
      expect(await File(path).exists(), false);
      expect(
        await startupEnabled(
          environment: {'HOME': home.path},
          windows: false,
          macos: false,
        ),
        false,
      );
    },
  );

  test('macOS launch agent is token-free and user scoped', () async {
    final home = await Directory.systemTemp.createTemp('rustfrp-launch-agent-');
    addTearDown(() => home.delete(recursive: true));
    final calls = <List<String>>[];
    Future<ProcessResult> run(String executable, List<String> arguments) async {
      calls.add([executable, ...arguments]);
      return ProcessResult(1, 0, executable == '/usr/bin/id' ? '501' : '', '');
    }

    await setStartupEnabled(
      true,
      appPath: '/Applications/RustFRP.app/Contents/MacOS/rustfrp_app',
      environment: {'HOME': home.path},
      windows: false,
      macos: true,
      run: run,
    );
    final path = startupDefinitionPath(
      home: home.path,
      windows: false,
      macos: true,
    );
    final text = await File(path).readAsString();
    expect(text, contains('com.rustfrp.desktop'));
    expect(text, contains('<string>--startup-to-tray</string>'));
    expect(text, isNot(contains('enroll-token')));
    expect(calls.single, ['/bin/chmod', '600', path]);
    await setStartupEnabled(
      false,
      appPath: '/Applications/RustFRP.app/Contents/MacOS/rustfrp_app',
      environment: {'HOME': home.path},
      windows: false,
      macos: true,
      run: run,
    );
    expect(await File(path).exists(), false);
    expect(calls.last, [
      '/bin/launchctl',
      'bootout',
      'gui/501/com.rustfrp.desktop',
    ]);
  });

  test('Windows task startup checks, creates, deletes and reports failure', () async {
    final calls = <List<String>>[];
    var exitCode = 0;
    Future<ProcessResult> run(String executable, List<String> arguments) async {
      calls.add([executable, ...arguments]);
      return ProcessResult(1, exitCode, '', 'denied');
    }

    final environment = {'USERPROFILE': r'C:\Users\tester'};
    expect(
      await startupEnabled(environment: environment, windows: true, run: run),
      true,
    );
    expect(calls.last, ['schtasks.exe', '/Query', '/TN', 'RustFRP-Startup']);
    await setStartupEnabled(
      true,
      appPath:
          '${Directory.systemTemp.path}${Platform.pathSeparator}RustFRP.exe',
      environment: environment,
      windows: true,
      run: run,
    );
    expect(calls.last, containsAll(['/Create', '/SC', 'ONLOGON', '/TN']));
    expect(
      calls.last.any((argument) => argument.contains('--startup-to-tray')),
      true,
    );
    exitCode = 1;
    await setStartupEnabled(
      false,
      appPath:
          '${Directory.systemTemp.path}${Platform.pathSeparator}RustFRP.exe',
      environment: environment,
      windows: true,
      run: run,
    );
    expect(calls.last, [
      'schtasks.exe',
      '/Delete',
      '/TN',
      'RustFRP-Startup',
      '/F',
    ]);
    expect(
      await startupEnabled(environment: environment, windows: true, run: run),
      false,
    );
    await expectLater(
      setStartupEnabled(
        true,
        appPath:
            '${Directory.systemTemp.path}${Platform.pathSeparator}RustFRP.exe',
        environment: environment,
        windows: true,
        run: run,
      ),
      throwsA(isA<FileSystemException>()),
    );
    await expectLater(
      setStartupEnabled(
        true,
        appPath: 'relative.exe',
        environment: environment,
        windows: true,
        run: run,
      ),
      throwsA(isA<FileSystemException>()),
    );
  });

  test('failed startup file permissions remove the staged file', () async {
    final home = await Directory.systemTemp.createTemp(
      'rustfrp-startup-error-',
    );
    addTearDown(() => home.delete(recursive: true));
    final path = startupDefinitionPath(
      home: home.path,
      windows: false,
      macos: false,
    );
    await expectLater(
      setStartupEnabled(
        true,
        appPath: '${home.path}${Platform.pathSeparator}RustFRP',
        environment: {'HOME': home.path},
        windows: false,
        macos: false,
        run: (_, _) async => ProcessResult(1, 1, '', 'chmod denied'),
      ),
      throwsA(isA<FileSystemException>()),
    );
    expect(await File(path).exists(), false);
  });

  test('default platform selection stays scoped to the supplied user directory', () async {
    final home = await Directory.systemTemp.createTemp(
      'rustfrp-startup-default-',
    );
    addTearDown(() => home.delete(recursive: true));
    final environment = {
      Platform.isWindows ? 'USERPROFILE' : 'HOME': home.path,
    };
    final calls = <List<String>>[];
    Future<ProcessResult> run(String executable, List<String> arguments) async {
      calls.add([executable, ...arguments]);
      return ProcessResult(1, 0, executable == '/usr/bin/id' ? '501' : '', '');
    }

    final executable =
        '${home.path}${Platform.pathSeparator}RustFRP${Platform.pathSeparator}app';
    await setStartupEnabled(
      true,
      appPath: executable,
      environment: environment,
      run: run,
    );
    expect(await startupEnabled(environment: environment, run: run), true);
    await setStartupEnabled(
      false,
      appPath: executable,
      environment: environment,
      run: run,
    );
    expect(await startupEnabled(environment: environment, run: run), false);
    expect(calls, isNotEmpty);
  });
}
