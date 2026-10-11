import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/local_agent.dart';
import 'package:rustfrp_app/core/platform/local_backend_io.dart';

class Rig {
  late Directory dir;
  ServerSocket? server;
  String reply =
      '{"version":1,"pid":123,"connected":true,"revision":7,"proxies":2}\n';
  bool closeOnStop = true;
  String? stopReply;
  String? logsReply;
  final requests = <Map<String, dynamic>>[];
  LocalAgentConfig get config => LocalAgentConfig(
    binary: '${dir.path}/agent',
    controller: 'host:7000',
    stateDir: '${dir.path}/state',
    name: "中文 ' \" \$literal",
    caFile: '${dir.path}/ca.pem',
  );
  Future<void> init() async {
    dir = await Directory.systemTemp.createTemp('rustfrp-local-dart-');
    await File(config.binary).writeAsString('fake');
    await Directory(config.stateDir).create();
  }

  Future<void> serve({String? discoveryIdentity}) async {
    await File('${config.stateDir}/identity.json')
        .writeAsString('{"client_id":"enrolled"}');
    server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    server!.listen((socket) async {
      final line = await socket
          .cast<List<int>>()
          .transform(utf8.decoder)
          .transform(const LineSplitter())
          .first;
      final request = jsonDecode(line) as Map<String, dynamic>;
      requests.add(request);
      if (request['command'] == 'stop') {
        socket.write(stopReply ?? '{"ok":true}\n');
        await socket.flush();
        if (closeOnStop) {
          await server!.close();
          server = null;
        }
      } else {
        socket.write(
          request['command'] == 'logs' ? (logsReply ?? reply) : reply,
        );
        await socket.flush();
      }
      await socket.close();
    });
    await File(config.discoveryPath).writeAsString(
      jsonEncode({
        'version': 1,
        'port': server!.port,
        'controller': discoveryIdentity ?? config.discoveryIdentity,
        'token': 'a' * 64,
      }),
    );
  }

  IoLocalBackend backend({
    LaunchAgent? launch,
    Duration timeout = const Duration(milliseconds: 150),
  }) => IoLocalBackend(defaults: config, launch: launch, startTimeout: timeout);
  Future<void> close() async {
    await server?.close();
    await dir.delete(recursive: true);
  }
}

void main() {
  test(
    'macOS token-free bootstrap, escaped arguments, permissions and cleanup',
    () async {
      expect(createLocalBackend(macos: true).available, true);
      expect(createLocalBackend(macos: false).available, true);
      expect(macJobLabel('/a'), macJobLabel('/a'));
      expect(macJobLabel('/a'), isNot(macJobLabel('/b')));
      final dir = await Directory.systemTemp.createTemp('rustfrp-mac-launch-');
      addTearDown(() => dir.delete(recursive: true));
      final path = '${dir.path}/control.json';
      final args = ['--local-control', path, '--name', '中文 <a> & "quoted"'];
      final calls = <List<String>>[];
      Future<ProcessResult> run(String binary, List<String> argv) async {
        if (binary == '/usr/bin/id') {
          expect(argv, ['-u']);
          return ProcessResult(1, 0, '501', '');
        }
        calls.add([binary, ...argv]);
        return ProcessResult(1, 0, '', '');
      }

      await launchMacAgent('/agent path', args, run: run);
      expect(calls[0], [
        '/bin/launchctl',
        'bootout',
        'gui/501/${macJobLabel(path)}',
      ]);
      expect(calls[1], ['/bin/chmod', '600', '$path.launchd.plist.tmp']);
      expect(calls[2], [
        '/bin/launchctl',
        'bootstrap',
        'gui/501',
        '$path.launchd.plist',
      ]);
      final plist = await File('$path.launchd.plist').readAsString();
      expect(plist, contains('&lt;a&gt; &amp; &quot;quoted&quot;'));
      expect(plist, contains('<key>KeepAlive</key><false/>'));
      expect(plist, isNot(contains('--enroll-token')));
      expect(
        macJobPlist('/a', ['--name', '--enroll-token'], 'l'),
        contains('--enroll-token'),
      );
      expect(
        () => macJobPlist('/a', ['--enroll-token', 'secret'], 'l'),
        throwsFormatException,
      );
      await removeMacJob(path, run: run);
      expect(calls.last, calls.first);
      for (final bad in [
        <String>[],
        ['--local-control'],
      ]) {
        await expectLater(
          launchMacAgent('/a', bad, run: run),
          throwsFormatException,
        );
      }
      await expectLater(
        removeMacJob(
          path,
          run: (_, _) async => ProcessResult(1, 0, 'invalid', ''),
        ),
        throwsFormatException,
      );
      await expectLater(
        launchMacAgent(
          '/a',
          args,
          run: (exe, argv) async => exe == '/usr/bin/id'
              ? ProcessResult(1, 0, '501', '')
              : ProcessResult(1, 1, '', ''),
        ),
        throwsFormatException,
      );
      expect(await File('$path.launchd.plist.tmp').exists(), false);
      await expectLater(
        launchMacAgent(
          '/a',
          args,
          run: (exe, argv) async => ProcessResult(
            1,
            argv.first == 'bootstrap' ? 1 : 0,
            exe == '/usr/bin/id' ? '501' : '',
            '',
          ),
        ),
        throwsFormatException,
      );
      if (Platform.isMacOS) {
        await launchMacAgent('/usr/bin/true', ['--local-control', path]);
        expect((await File('$path.launchd.plist').stat()).mode & 0x1ff, 0x180);
        await removeMacJob(path);
      } else {
        await expectLater(removeMacJob(path), throwsA(isA<ProcessException>()));
      }
    },
  );
  test(
    'first enrollment completes before token-free macOS daemon starts',
    () async {
      final r = Rig();
      await r.init();
      addTearDown(r.close);
      final sequence = <String>[];
      final backend = IoLocalBackend(
        defaults: r.config,
        startTimeout: const Duration(seconds: 1),
        firstLaunch: (binary, args) async {
          sequence.add('enroll');
          expect(args, contains('--enroll-token'));
          await r.serve();
        },
        launch: (binary, args) async {
          sequence.add('daemon');
          expect(args, isNot(contains('--enroll-token')));
          await r.serve();
        },
        removeRegistration: (path) async {
          sequence.add('cleanup');
          expect(path, r.config.discoveryPath);
        },
      );
      await backend.start(r.config, 'test-only');
      expect(sequence, ['enroll', 'cleanup', 'daemon']);
      expect((await backend.status(r.config))!.connected, true);
      await backend.stop(r.config);
      expect(sequence.last, 'cleanup');
      final timeout = IoLocalBackend(
        defaults: r.config,
        startTimeout: const Duration(milliseconds: 1),
        launch: (_, _) async {},
        removeRegistration: (_) async {
          sequence.add('timeout-cleanup');
        },
      );
      await expectLater(timeout.start(r.config, ''), throwsFormatException);
      expect(sequence.last, 'timeout-cleanup');
    },
  );
  test('startup retries token rotation, but ordinary status rejects bad authentication', () async {
    final r = Rig();
    await r.init();
    addTearDown(r.close);
    final backend = r.backend(
      timeout: const Duration(seconds: 1),
      launch: (_, _) async {
        await r.serve();
        r.reply = '{"error":"unauthorized"}\n';
        Future<void>.delayed(const Duration(milliseconds: 30), () {
          r.reply = '{"version":1,"pid":123,"connected":true,"revision":7,"proxies":2}\n';
        });
      },
    );
    await backend.start(r.config, 'test-token');
    expect((await backend.status(r.config))!.connected, true);
    r.reply = '{"error":"unauthorized"}\n';
    await expectLater(backend.status(r.config), throwsFormatException);
    r.reply =
        '{"version":1,"pid":123,"connected":true,"revision":7,"proxies":2}\n';
    await backend.stop(r.config);
  });
  test(
    'native factory, defaults for all platforms, real detached launch',
    () async {
      expect(createLocalBackend().available, true);
      final unix = desktopDefaults({
        'HOME': '/h',
        'HOSTNAME': 'nas',
      }, windows: false);
      expect(unix.binary, '/h/.rustfrp/bin/rustfrp-agent');
      expect(unix.name, 'nas');
      final windows = desktopDefaults({
        'LOCALAPPDATA': 'C:/Users/me',
        'COMPUTERNAME': 'pc',
      }, windows: true);
      expect(windows.binary, 'C:/Users/me/RustFRP/bin/rustfrp-agent.exe');
      expect(windows.name, 'pc');
      expect(desktopDefaults({}, windows: false).name, '本机');
      expect(desktopDefaults({}, windows: true).stateDir, './RustFRP/desktop');
      final dir = await Directory.systemTemp.createTemp('rustfrp-detach');
      try {
        await launchDetached('/usr/bin/touch', ['${dir.path}/marker']);
        for (
          var i = 0;
          i < 50 && !await File('${dir.path}/marker').exists();
          i++
        ) {
          await Future<void>.delayed(const Duration(milliseconds: 10));
        }
        expect(await File('${dir.path}/marker').exists(), true);
      } finally {
        await dir.delete(recursive: true);
      }
    },
  );
  test('bundled agent paths and first-run installation', () async {
    final sep = Platform.pathSeparator;
    final root = Directory.systemTemp.path;
    final windowsExe = '$root${sep}bundle${sep}app${sep}rustfrp_app.exe';
    final macExe =
        '$root${sep}RustFRP.app${sep}Contents${sep}MacOS${sep}rustfrp_app';
    final linuxExe = '$root${sep}RustFRP${sep}app${sep}rustfrp_app';
    expect(
      bundledAgentPathFor(
        windowsExe,
        windows: true,
        macos: false,
        linux: false,
      ),
      '${File(windowsExe).parent.path}${sep}rustfrp-agent.exe',
    );
    expect(
      bundledAgentPathFor(macExe, windows: false, macos: true, linux: false),
      '${File(macExe).parent.parent.path}${sep}Resources${sep}bin${sep}rustfrp-agent',
    );
    expect(
      bundledAgentPathFor(linuxExe, windows: false, macos: false, linux: true),
      '${File(linuxExe).parent.parent.path}${sep}bin${sep}rustfrp-agent',
    );
    expect(
      bundledAgentPathFor(linuxExe, windows: false, macos: false, linux: false),
      isNull,
    );

    final r = Rig();
    await r.init();
    addTearDown(r.close);
    await File(r.config.binary).delete();
    final source = File('${r.dir.path}${sep}packaged-agent');
    await source.writeAsString('bundled agent');
    final backend = IoLocalBackend(
      defaults: r.config,
      bundledAgentBinary: source.path,
      launch: (binary, _) async {
        expect(await File(binary).readAsString(), 'bundled agent');
        await r.serve();
      },
    );
    await backend.start(r.config, 'test-token');
    expect(await File(r.config.binary).readAsString(), 'bundled agent');
    if (!Platform.isWindows) {
      final permissions = await Process.run(
        '/usr/bin/stat',
        Platform.isMacOS
            ? ['-f', '%Lp', r.config.binary]
            : ['-c', '%a', r.config.binary],
      );
      expect(permissions.stdout.toString().trim(), '700');
    }
    await backend.stop(r.config);

    final missing = IoLocalBackend(
      defaults: r.config,
      bundledAgentBinary: '${r.dir.path}${sep}missing-agent',
    );
    await File(r.config.binary).delete();
    await expectLater(
      missing.start(r.config, 'test-token'),
      throwsA(isA<FormatException>()),
    );
  });
  test('status, stopped discovery, start and stop; literal arguments and no duplicate', () async {
    final r = Rig();
    await r.init();
    addTearDown(r.close);
    final b = r.backend(
      launch: (binary, args) async {
        expect(binary, r.config.binary);
        expect(args[args.indexOf('--name') + 1], r.config.name);
        expect(args[args.indexOf('--enroll-token') + 1], 't');
        expect(args, contains('--ca-file'));
        expect(
          args[args.indexOf('--local-control') + 1],
          r.config.discoveryPath,
        );
        await r.serve();
      },
    );
    expect(await b.status(r.config), isNull);
    await b.stop(r.config);
    await b.start(r.config, ' t ');
    expect((await b.status(r.config))!.connected, true);
    await b.start(r.config, 'ignored');
    expect(r.requests.every((v) => v['token'] == 'a' * 64), true);
    await b.stop(r.config);
    expect(await b.status(r.config), isNull);
    await File('${r.config.stateDir}/identity.json')
        .writeAsString('{"client_id":"device-id"}');
    final enrolled = r.backend(
      launch: (binary, args) async {
        expect(args, isNot(contains('--enroll-token')));
        await r.serve();
      },
    );
    await enrolled.start(
      LocalAgentConfig(
        binary: r.config.binary,
        controller: r.config.controller,
        stateDir: r.config.stateDir,
      ),
      'never-used',
    );
    await enrolled.stop(r.config);
  });
  test('independent mode validates a local frpc file and starts without enrollment', () async {
    final r = Rig();
    await r.init();
    addTearDown(r.close);
    final configPath = '${r.dir.path}/frpc.toml';
    await File(configPath).writeAsString('serverAddr="127.0.0.1"');
    final config = LocalAgentConfig(
      binary: r.config.binary,
      stateDir: r.config.stateDir,
      configFile: configPath,
      name: 'standalone',
    );
    final backend = IoLocalBackend(
      defaults: config,
      startTimeout: const Duration(seconds: 1),
      launch: (binary, args) async {
        expect(binary, config.binary);
        expect(args, contains('--local-control'));
        expect(args, contains('--mode'));
        expect(args, contains('client'));
        expect(args, contains('-c'));
        expect(args, contains(configPath));
        expect(args, isNot(contains('--controller')));
        expect(args, isNot(contains('--enroll-token')));
        await r.serve(discoveryIdentity: config.discoveryIdentity);
      },
    );
    await backend.start(config, 'unused');
    expect((await backend.status(config))!.connected, true);
    await backend.stop(config);
  });
  test(
    'invalid paths, controller, credentials, discovery and bounded file reads',
    () async {
      final r = Rig();
      await r.init();
      addTearDown(r.close);
      final b = r.backend();
      for (final controller in [
        '',
        'http://host:7000',
        'x' * 301,
        'host:0',
        'host:65536',
      ]) {
        await expectLater(
          b.start(
            LocalAgentConfig(
              binary: r.config.binary,
              stateDir: r.config.stateDir,
              controller: controller,
            ),
            't',
          ),
          throwsFormatException,
        );
      }
      for (final c in [
        const LocalAgentConfig(
          binary: 'relative',
          stateDir: '/tmp',
          controller: 'host',
        ),
        const LocalAgentConfig(
          binary: '/tmp/x',
          stateDir: 'relative',
          controller: 'host',
        ),
        LocalAgentConfig(
          binary: '${r.dir.path}/missing',
          stateDir: r.config.stateDir,
          controller: 'host',
        ),
      ]) {
        await expectLater(b.start(c, 't'), throwsFormatException);
      }
      await expectLater(b.start(r.config, ''), throwsFormatException);
      for (final content in [
        'bad',
        'x' * 2049,
        jsonEncode({
          'version': 1,
          'port': 0,
          'token': 'a' * 64,
          'controller': 'host:7000',
        }),
        jsonEncode({
          'version': 1,
          'port': 65536,
          'token': 'a' * 64,
          'controller': 'host:7000',
        }),
        jsonEncode({
          'version': 1,
          'port': 1,
          'token': 'bad',
          'controller': 'host:7000',
        }),
      ]) {
        await File(r.config.discoveryPath).writeAsString(content);
        await expectLater(b.status(r.config), throwsA(isA<FormatException>()));
      }
      await File(r.config.discoveryPath).delete();
      await Directory(r.config.discoveryPath).create();
      await expectLater(
        b.status(r.config),
        throwsA(isA<FileSystemException>()),
      );
      await Directory(r.config.discoveryPath).delete();
      await expectLater(
        r.backend(launch: (_, _) async {}).start(r.config, 't'),
        throwsFormatException,
      );
    },
  );
  test('protocol rejects unauthorized, oversized, incomplete, unsupported responses and stop failure', () async {
    final r = Rig();
    await r.init();
    await r.serve();
    addTearDown(r.close);
    final b = r.backend();
    for (final reply in [
      '{"error":"unauthorized"}\n',
      '[]\n',
      'x' * 2049,
      '',
      '{"pid":1}\n',
      '{}',
    ]) {
      r.reply = reply;
      await expectLater(b.status(r.config), throwsFormatException);
    }
    r.reply =
        '{"version":1,"pid":1,"connected":false,"revision":0,"proxies":0}\n';
    r.stopReply = '{}\n';
    r.closeOnStop = false;
    await expectLater(b.stop(r.config), throwsFormatException);
    r.stopReply = '{"ok":true}\n';
    await expectLater(b.stop(r.config), throwsFormatException);
  });
  test(
    'private logs are bounded, validated and old agent is explicit',
    () async {
      final r = Rig();
      await r.init();
      addTearDown(r.close);
      final b = r.backend();
      await expectLater(b.logs(r.config), throwsFormatException);
      await r.serve();
      r.reply =
          '${jsonEncode({
            'items': ['hello', '中' * 100],
          })}\n';
      expect(await b.logs(r.config), ['hello', '中' * 100]);
      expect(r.requests.last['command'], 'logs');
      for (final items in [
        null,
        [1],
        List.filled(129, 'x'),
        ['x' * 513],
      ]) {
        r.reply = '${jsonEncode({'items': items})}\n';
        await expectLater(b.logs(r.config), throwsFormatException);
      }
      r.reply = '{"error":"invalid_request"}\n';
      await expectLater(
        b.logs(r.config),
        throwsA(
          isA<FormatException>().having(
            (e) => e.message,
            'message',
            contains('更新 agent'),
          ),
        ),
      );
    },
  );
  test('diagnostic export writes private allowlisted snapshot', () async {
    final r = Rig();
    await r.init();
    addTearDown(r.close);
    final b = r.backend();
    await expectLater(b.exportDiagnostics(r.config), throwsFormatException);
    await r.serve();
    r.logsReply =
        '${jsonEncode({
          'items': ['connected', 'token=hidden', 'path /private/file'],
        })}\n';
    final path = await b.exportDiagnostics(r.config);
    final file = File(path);
    final contents = await file.readAsString();
    final json = jsonDecode(contents) as Map<String, dynamic>;
    expect(json['schema_version'], 1);
    expect(json['logs'], ['connected', '[redacted]', '[redacted]']);
    expect(contents, isNot(contains('host:7000')));
    expect(contents, isNot(contains('token=hidden')));
    expect(json['agent_version'], '未知');
    if (!Platform.isWindows) {
      final permissions = await Process.run(
        '/usr/bin/stat',
        Platform.isMacOS ? ['-f', '%Lp', path] : ['-c', '%a', path],
      );
      expect(permissions.stdout.toString().trim(), '600');
    }
    final next = await b.exportDiagnostics(r.config);
    expect(next, isNot(path));
    expect(await file.readAsString(), contents);
  });
  test('tunnel pagination validates revision and capacity', () async {
    final r = Rig();
    await r.init();
    addTearDown(r.close);
    final b = r.backend();
    await expectLater(b.tunnels(r.config), throwsFormatException);
    await r.serve();
    final item = {
      'id': 'id',
      'kind': 'tcp',
      'local_ip': 'localhost',
      'local_port': 22,
      'remote_port': 0,
    };
    r.reply =
        '${jsonEncode({
          'revision': 1,
          'items': [item],
          'next_offset': null,
        })}\n';
    expect(await b.tunnels(r.config), [item]);
    for (final value in [
      {'revision': 'x', 'items': []},
      {
        'revision': 1,
        'items': [{}],
      },
      {'revision': 1, 'items': List.filled(33, item)},
      {'revision': 1, 'items': [], 'next_offset': 1},
      {'revision': 1, 'items': [], 'next_offset': 'x'},
    ]) {
      r.reply = '${jsonEncode(value)}\n';
      await expectLater(b.tunnels(r.config), throwsFormatException);
    }
    r.reply =
        '${jsonEncode({'revision': 1, 'items': [], 'next_offset': 32})}\n';
    await expectLater(b.tunnels(r.config), throwsFormatException);
  });
  test('desktop certificate pin is validated and passed to launch', () async {
    final r = Rig();
    await r.init();
    addTearDown(r.close);
    List<String>? launched;
    final backend = r.backend(
      launch: (_, args) async {
        launched = args;
        await r.serve();
      },
    );
    final base = r.config.toJson();
    final invalid = LocalAgentConfig.fromJson({...base, 'tlsPin': 'invalid'});
    await expectLater(
      backend.start(invalid, 'test-token'),
      throwsFormatException,
    );
    expect(launched, isNull);
    final valid = LocalAgentConfig.fromJson({...base, 'tlsPin': 'a' * 64});
    await backend.start(valid, 'test-token');
    expect(launched, contains('--tls-pin'));
    expect(launched, contains('a' * 64));
  });
}
