import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter/foundation.dart' show visibleForTesting;

import '../local_agent.dart';

LocalAgentBackend createLocalBackend({bool? macos}) => IoLocalBackend(
  launch: (macos ?? Platform.isMacOS) ? launchMacAgent : launchDetached,
  firstLaunch: (macos ?? Platform.isMacOS) ? launchDetached : null,
  removeRegistration: (macos ?? Platform.isMacOS) ? removeMacJob : null,
  defaults: desktopDefaults(Platform.environment, windows: Platform.isWindows),
);

LocalAgentConfig desktopDefaults(
  Map<String, String> env, {
  required bool windows,
}) {
  final root = windows
      ? '${env['LOCALAPPDATA'] ?? '.'}/RustFRP'
      : '${env['HOME'] ?? '.'}/.rustfrp';
  return LocalAgentConfig(
    binary: '$root/bin/rustfrp-agent${windows ? '.exe' : ''}',
    stateDir: '$root/desktop',
    name: env['COMPUTERNAME'] ?? env['HOSTNAME'] ?? '本机',
  );
}

typedef LaunchAgent = Future<void> Function(
  String binary,
  List<String> arguments,
);
Future<void> launchDetached(String binary, List<String> arguments) async {
  await Process.start(
    binary,
    arguments,
    mode: ProcessStartMode.detached,
    runInShell: false,
  );
}

typedef RunCommand = Future<ProcessResult> Function(
  String executable,
  List<String> arguments,
);
String macJobLabel(String discoveryPath) =>
    'com.rustfrp.desktop.${sha256.convert(utf8.encode(discoveryPath))}';

Future<String> _macUid(RunCommand invoke) async {
  final result = await invoke('/usr/bin/id', [
    '-u',
  ]).timeout(const Duration(seconds: 5));
  final uid = int.tryParse(result.stdout.toString().trim());
  if (result.exitCode != 0 || uid == null || uid < 0) {
    throw const FormatException('无法确定 macOS 用户会话');
  }
  return '$uid';
}

Future<void> removeMacJob(String discoveryPath, {RunCommand? run}) async {
  final invoke = run ?? ((exe, args) => Process.run(exe, args));
  final uid = await _macUid(invoke);
  await invoke('/bin/launchctl', [
    'bootout',
    'gui/$uid/${macJobLabel(discoveryPath)}',
  ]).timeout(const Duration(seconds: 5));
}

String macJobPlist(String binary, List<String> arguments, String label) {
  if (arguments.asMap().entries.any(
    (e) => e.key.isEven && e.value == '--enroll-token',
  )) {
    throw const FormatException('后台服务不能保存接入令牌，请先完成接入');
  }
  String xml(String value) => const HtmlEscape().convert(value);
  final argv = [
    binary,
    ...arguments,
  ].map((a) => '<string>${xml(a)}</string>').join();
  return '<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>Label</key><string>${xml(label)}</string><key>ProgramArguments</key><array>$argv</array><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>';
}

/// Explicit GUI domain and a token-free definition in the state directory.
/// The job lives through app exit; the file is not a login/startup LaunchAgent.
Future<void> launchMacAgent(
  String binary,
  List<String> arguments, {
  RunCommand? run,
}) async {
  final invoke = run ?? ((exe, args) => Process.run(exe, args));
  if (arguments.length < 2 || arguments.first != '--local-control') {
    throw const FormatException('本机控制路径缺失');
  }
  final path = arguments[1];
  final label = macJobLabel(path);
  final plist = macJobPlist(binary, arguments, label);
  final uid = await _macUid(invoke);
  await invoke('/bin/launchctl', [
    'bootout',
    'gui/$uid/$label',
  ]).timeout(const Duration(seconds: 5));
  final file = File('$path.launchd.plist');
  await file.parent.create(recursive: true);
  final staging = File('${file.path}.tmp');
  await staging.writeAsString(plist);
  final permissions = await invoke('/bin/chmod', [
    '600',
    staging.path,
  ]).timeout(const Duration(seconds: 5));
  if (permissions.exitCode != 0) {
    await staging.delete();
    throw const FormatException('无法保护 macOS 后台服务文件');
  }
  await staging.rename(file.path);
  final result = await invoke('/bin/launchctl', [
    'bootstrap',
    'gui/$uid',
    file.path,
  ]).timeout(const Duration(seconds: 5));
  if (result.exitCode != 0) {
    throw const FormatException('无法交给 macOS 后台运行，请检查 agent 路径与用户会话');
  }
}

/// Windows 使用当前用户 LOCALAPPDATA；POSIX 发现文件由 agent 强制 0600。
class IoLocalBackend implements LocalAgentBackend {
  IoLocalBackend({
    required this.defaults,
    LaunchAgent? launch,
    this.firstLaunch,
    this.removeRegistration,
    this.bundledAgentBinary,
    this.startTimeout = const Duration(seconds: 10),
  }) : _launch = launch ?? launchDetached;
  @override
  final LocalAgentConfig defaults;
  final LaunchAgent _launch;
  final LaunchAgent? firstLaunch;
  final Future<void> Function(String path)? removeRegistration;
  final String? bundledAgentBinary;
  final Duration startTimeout;
  @override
  bool get available => true;

  String? _bundledAgentPath() {
    if (bundledAgentBinary != null) return bundledAgentBinary;
    return bundledAgentPathFor(
      Platform.resolvedExecutable,
      windows: Platform.isWindows,
      macos: Platform.isMacOS,
      linux: Platform.isLinux,
    );
  }

  Future<void> _installBundledAgent(LocalAgentConfig config) async {
    final sourcePath = _bundledAgentPath();
    if (sourcePath == null || !await File(sourcePath).exists()) {
      throw const FormatException('找不到随应用提供的 agent，请重新安装完整桌面包');
    }
    final destination = File(config.binary);
    await destination.parent.create(recursive: true);
    await File(sourcePath).copy(destination.path);
    if (!Platform.isWindows) {
      final result = await Process.run('/bin/chmod', ['700', destination.path]);
      if (result.exitCode != 0) {
        await destination.delete();
        throw const FormatException('无法设置 agent 可执行权限');
      }
    }
  }

  Future<Map<String, dynamic>?> _read(String path, {int limit = 2048}) async {
    RandomAccessFile file;
    try {
      file = await File(path).open();
    } on FileSystemException catch (e) {
      if ({2, 3}.contains(e.osError?.errorCode)) return null;
      rethrow;
    }
    try {
      final bytes = await file.read(limit + 1);
      if (bytes.length > limit) throw const FormatException('本机状态文件过大，请检查状态目录');
      final decoded = jsonDecode(utf8.decode(bytes));
      if (decoded is! Map<String, dynamic>) {
        throw const FormatException('本机状态文件格式不兼容');
      }
      return decoded;
    } finally {
      await file.close();
    }
  }

  Future<Map<String, dynamic>?> _discovery(LocalAgentConfig config) async {
    final d = await _read(config.discoveryPath);
    if (d == null) return null;
    if (d['version'] != 1 ||
        d['controller'] != config.discoveryIdentity ||
        d['port'] is! int ||
        (d['port'] as int) < 1 ||
        (d['port'] as int) > 65535 ||
        d['token'] is! String ||
        !RegExp(r'^[0-9a-f]{64}$').hasMatch(d['token'] as String)) {
      throw const FormatException('状态目录属于其他控制器或格式不兼容，请使用独立的状态目录');
    }
    return d;
  }

  Future<Map<String, dynamic>> _request(
    Map<String, dynamic> d,
    String command, {
    int limit = 2048,
    int? offset,
  }) async {
    final socket = await Socket.connect(
      InternetAddress.loopbackIPv4,
      d['port'] as int,
      timeout: const Duration(seconds: 2),
    );
    try {
      socket.write(
        '${jsonEncode({'token': d['token'], 'command': command, 'offset': ?offset})}\n',
      );
      await socket.flush().timeout(const Duration(seconds: 2));
      final bytes = <int>[];
      await for (final chunk in socket.timeout(const Duration(seconds: 3))) {
        if (bytes.length + chunk.length > limit) {
          throw const FormatException('本机 agent 响应过大');
        }
        bytes.addAll(chunk);
        if (bytes.contains(10)) break;
      }
      if (bytes.isEmpty || bytes.last != 10) {
        throw const FormatException('本机 agent 响应不完整');
      }
      final result = jsonDecode(utf8.decode(bytes));
      if (result is! Map<String, dynamic>) {
        throw const FormatException('本机 agent 响应格式不兼容');
      }
      if (result['error'] == 'invalid_request' &&
          (command == 'logs' || command == 'tunnels')) {
        throw const FormatException('请更新 agent 以查看本机日志');
      }
      if (result.containsKey('error')) {
        throw const FormatException('本机控制认证失败，请检查 agent 与状态目录');
      }
      return result;
    } finally {
      socket.destroy();
    }
  }

  @override
  Future<String> exportDiagnostics(LocalAgentConfig config) async {
    final current = await status(config);
    if (current == null) throw const FormatException('agent 未运行');
    final contents = diagnosticJson(
      current,
      await logs(config),
      DateTime.now(),
    );
    final destination =
        '${config.stateDir}/diagnostics-${DateTime.now().microsecondsSinceEpoch}.json';
    final file = File(destination);
    await file.create(exclusive: true);
    try {
      if (!Platform.isWindows) {
        final result = await Process.run('/bin/chmod', ['600', destination]);
        if (result.exitCode != 0) throw const FileSystemException('无法设置诊断文件权限');
      }
      await file.writeAsString(contents, flush: true);
      return destination;
    } catch (_) {
      await file.delete();
      rethrow;
    }
  }

  @override
  Future<List<Map<String, dynamic>>> tunnels(LocalAgentConfig config) async {
    final d = await _discovery(config);
    if (d == null) throw const FormatException('agent 未运行');
    final items = <Map<String, dynamic>>[];
    int? revision;
    var offset = 0;
    for (var page = 0; page < 32; page++) {
      final reply = await _request(d, 'tunnels', limit: 150000, offset: offset);
      final batch = reply['items'];
      if (reply['revision'] is! int ||
          (revision != null && revision != reply['revision']) ||
          batch is! List ||
          batch.length > 32 ||
          batch.any(
            (item) =>
                item is! Map<String, dynamic> ||
                ['id', 'kind', 'local_ip'].any(
                  (key) =>
                      item[key] is! String ||
                      utf8.encode(item[key] as String).length > 256,
                ) ||
                item['local_port'] is! int ||
                item['remote_port'] is! int,
          )) {
        throw const FormatException('本机隧道配置变化或响应不兼容，请刷新');
      }
      revision = reply['revision'] as int;
      items.addAll(batch.cast<Map<String, dynamic>>());
      final next = reply['next_offset'];
      if (next == null) return List.unmodifiable(items);
      if (next is! int || next != offset + 32) {
        throw const FormatException('本机隧道分页不兼容');
      }
      offset = next;
    }
    throw const FormatException('本机隧道超过容量限制');
  }

  @override
  Future<List<String>> logs(LocalAgentConfig config) async {
    final d = await _discovery(config);
    if (d == null) throw const FormatException('agent 未运行');
    final reply = await _request(d, 'logs', limit: 400000);
    final items = reply['items'];
    if (items is! List ||
        items.length > 128 ||
        items.any(
          (line) => line is! String || utf8.encode(line).length > 512,
        )) {
      throw const FormatException('本机日志格式不兼容');
    }
    return List<String>.unmodifiable(items.cast<String>());
  }

  @override
  Future<LocalAgentStatus?> status(LocalAgentConfig config) async {
    final d = await _discovery(config);
    if (d == null) return null;
    try {
      return LocalAgentStatus.fromJson(await _request(d, 'status'));
    } on SocketException catch (e) {
      // Only connection refusal means a stopped process. Timeouts / other errors must not
      // be treated as permission to launch a duplicate agent.
      if ({61, 111, 10061}.contains(e.osError?.errorCode)) return null;
      rethrow;
    }
  }

  @override
  Future<void> start(LocalAgentConfig config, String enrollToken) async {
    final standalone = config.configFile.isNotEmpty;
    if (standalone) {
      if (!File(config.configFile).isAbsolute ||
          !await File(config.configFile).exists()) {
        throw const FormatException('独立配置文件必须存在且使用绝对路径');
      }
    } else {
      if (config.controller.length > 300 ||
          !RegExp(r'^[a-zA-Z0-9][a-zA-Z0-9.-]*(:[0-9]+)?$')
              .hasMatch(config.controller)) {
        throw const FormatException(
          '请输入 Edge 地址，例如 frp.example.com:7000，不带 http://',
        );
      }
      final port = config.controller.contains(':')
          ? int.tryParse(config.controller.split(':').last)
          : 7000;
      if (port == null || port < 1 || port > 65535) {
        throw const FormatException('Edge 端口必须在 1–65535 之间');
      }
    }
    if (!File(config.binary).isAbsolute ||
        !Directory(config.stateDir).isAbsolute) {
      throw const FormatException('agent 与状态目录必须使用绝对路径');
    }
    if (await status(config) != null) return;
    if (!await File(config.binary).exists()) await _installBundledAgent(config);
    final identity = standalone
        ? null
        : await _read('${config.stateDir}/identity.json', limit: 16384);
    if (!standalone &&
        identity?['client_id'] == null &&
        enrollToken.trim().isEmpty) {
      throw const FormatException('首次接入需要面板生成的一次性令牌');
    }
    final args = ['--local-control', config.discoveryPath, '--mode', 'client'];
    if (standalone) {
      args.addAll(['-c', config.configFile]);
    } else {
      args.addAll([
        '--controller',
        config.controller,
        '--state-dir',
        config.stateDir,
        '--name',
        config.name,
      ]);
    }
    if (!standalone && config.caFile.isNotEmpty) {
      args.addAll(['--ca-file', config.caFile]);
    }
    if (!standalone && config.tlsPin.isNotEmpty) {
      if (!RegExp(r'^[0-9a-fA-F]{64}$').hasMatch(config.tlsPin)) {
        throw const FormatException('证书指纹必须为 64 位 SHA256');
      }
      args.addAll(['--tls-pin', config.tlsPin]);
    }
    final needsEnrollment = !standalone && identity?['client_id'] == null;
    final initialArgs = [
      ...args,
      if (needsEnrollment) ...['--enroll-token', enrollToken.trim()],
    ];
    final initialLauncher = firstLaunch;
    await (initialLauncher != null && needsEnrollment
        ? initialLauncher
        : _launch)(config.binary, initialArgs);
    await _waitForStart(config, needsEnrollment);
    if (firstLaunch != null && needsEnrollment) {
      await stop(config);
      await _launch(config.binary, args);
      await _waitForStart(config, false);
    }
  }

  Future<void> _waitForStart(LocalAgentConfig config, bool enrollment) async {
    final deadline = DateTime.now().add(startTimeout);
    while (DateTime.now().isBefore(deadline)) {
      try {
        final current = await status(config);
        if (current != null &&
            (!enrollment ||
                (current.connected &&
                    (await _read(
                          '${config.stateDir}/identity.json',
                          limit: 16384,
                        ))?['client_id'] !=
                        null))) {
          return;
        }
      } on FormatException {
        // Only after our own launch: discovery publication can rotate its token.
      }
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    await removeRegistration?.call(config.discoveryPath);
    throw const FormatException('agent 未能启动，请检查令牌、证书与二进制版本');
  }

  @override
  Future<void> stop(LocalAgentConfig config) async {
    final d = await _discovery(config);
    if (d == null) return;
    // Reuse the authenticated channel; never kill by an untrusted / recycled PID.
    if ((await _request(d, 'stop'))['ok'] != true) {
      throw const FormatException('agent 未确认停止请求');
    }
    final deadline = DateTime.now().add(startTimeout);
    while (DateTime.now().isBefore(deadline)) {
      if (await status(config) == null) {
        await removeRegistration?.call(config.discoveryPath);
        return;
      }
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    throw const FormatException('agent 仍在退出，请稍后刷新状态');
  }
}

@visibleForTesting
String? bundledAgentPathFor(
  String resolvedExecutable, {
  required bool windows,
  required bool macos,
  required bool linux,
}) {
  final executable = File(resolvedExecutable).absolute;
  if (windows) {
    return '${executable.parent.path}${Platform.pathSeparator}rustfrp-agent.exe';
  }
  if (macos) {
    final contents = executable.parent.parent;
    return '${contents.path}${Platform.pathSeparator}Resources${Platform.pathSeparator}bin${Platform.pathSeparator}rustfrp-agent';
  }
  if (linux) {
    final bundleRoot = executable.parent.parent;
    return '${bundleRoot.path}${Platform.pathSeparator}bin${Platform.pathSeparator}rustfrp-agent';
  }
  return null;
}
