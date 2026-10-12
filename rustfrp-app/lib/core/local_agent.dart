import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'platform/local_backend.dart';
import 'notification_service.dart';
import 'store.dart';

@immutable
class LocalAgentConfig {
  const LocalAgentConfig({
    this.binary = '',
    this.controller = '',
    this.name = '',
    this.stateDir = '',
    this.configFile = '',
    this.caFile = '',
    this.tlsPin = '',
  });
  final String binary, controller, name, stateDir, configFile, caFile, tlsPin;
  String get discoveryIdentity =>
      configFile.isEmpty ? controller : 'standalone:$configFile';
  String get discoveryPath => '$stateDir/control.json';
  Map<String, Object> toJson() => {
    'binary': binary,
    'controller': controller,
    'name': name,
    'stateDir': stateDir,
    'configFile': configFile,
    'caFile': caFile,
    'tlsPin': tlsPin,
  };
  factory LocalAgentConfig.fromJson(Map<String, dynamic> json) =>
      LocalAgentConfig(
        binary: json['binary'] as String,
        controller: json['controller'] as String,
        name: json['name'] as String,
        stateDir: json['stateDir'] as String,
        configFile: json['configFile'] as String? ?? '',
        caFile: json['caFile'] as String,
        tlsPin: json['tlsPin'] as String? ?? '',
      );
  @override
  bool operator ==(Object other) =>
      other is LocalAgentConfig &&
      binary == other.binary &&
      controller == other.controller &&
      name == other.name &&
      stateDir == other.stateDir &&
      configFile == other.configFile &&
      caFile == other.caFile &&
      tlsPin == other.tlsPin;
  @override
  int get hashCode => Object.hash(
    binary,
    controller,
    name,
    stateDir,
    configFile,
    caFile,
    tlsPin,
  );
}

@immutable
class LocalAgentStatus {
  const LocalAgentStatus({
    required this.pid,
    required this.connected,
    required this.revision,
    required this.proxies,
    this.agentVersion = '未知',
    this.bytesIn,
    this.bytesOut,
    this.connections,
  });
  final String agentVersion;
  final int pid, revision, proxies;
  final int? bytesIn, bytesOut, connections;
  final bool connected;
  factory LocalAgentStatus.fromJson(Map<String, dynamic> json) {
    if (json['version'] != 1 ||
        json['pid'] is! int ||
        json['connected'] is! bool ||
        json['revision'] is! int ||
        json['proxies'] is! int ||
        (json['pid'] as int) <= 0 ||
        (json['revision'] as int) < 0 ||
        (json['proxies'] as int) < 0) {
      throw const FormatException('本机 agent 返回了不支持的状态，请更新 agent');
    }
    final traffic = json['traffic'];
    if (traffic != null &&
        (traffic is! Map ||
            ['bytes_in', 'bytes_out', 'connections'].any(
              (key) => traffic[key] is! int || (traffic[key] as int) < 0,
            ))) {
      throw const FormatException('本机 agent 返回了无效的流量统计');
    }
    return LocalAgentStatus(
      agentVersion: json['agent_version'] is String
          ? json['agent_version'] as String
          : '未知',
      pid: json['pid'] as int,
      connected: json['connected'] as bool,
      revision: json['revision'] as int,
      proxies: json['proxies'] as int,
      bytesIn: traffic == null ? null : traffic['bytes_in'] as int,
      bytesOut: traffic == null ? null : traffic['bytes_out'] as int,
      connections: traffic == null ? null : traffic['connections'] as int,
    );
  }
}

abstract interface class LocalAgentBackend {
  bool get available;
  LocalAgentConfig get defaults;
  Future<LocalAgentStatus?> status(LocalAgentConfig config);
  Future<List<String>> logs(LocalAgentConfig config);
  Future<List<Map<String, dynamic>>> tunnels(LocalAgentConfig config);
  Future<String> exportDiagnostics(LocalAgentConfig config);
  Future<String> readConfig(LocalAgentConfig config);
  Future<void> saveConfig(LocalAgentConfig config, String text);
  Future<void> start(LocalAgentConfig config, String enrollToken);
  Future<void> stop(LocalAgentConfig config);
}

final localBackendProvider = Provider<LocalAgentBackend>(
  (ref) => createLocalBackend(),
);
final localAvailableProvider = Provider<bool>(
  (ref) =>
      !ref.watch(sameOriginProvider) &&
      ref.watch(localBackendProvider).available,
);

class LocalModeNotifier extends Notifier<bool> {
  @override
  bool build() =>
      ref.watch(localAvailableProvider) &&
      (ref.read(prefsProvider).getBool('local_mode') ?? false);
  Future<void> select(bool local) async {
    state = local && ref.read(localAvailableProvider);
    await ref.read(prefsProvider).setBool('local_mode', state);
  }
}

final localModeProvider = NotifierProvider<LocalModeNotifier, bool>(
  LocalModeNotifier.new,
);

@immutable
class LocalAgentState {
  const LocalAgentState({
    required this.config,
    this.status,
    this.busy = false,
    this.error,
  });
  final LocalAgentConfig config;
  final LocalAgentStatus? status;
  final bool busy;
  final String? error;
}

class LocalAgentNotifier extends Notifier<LocalAgentState> {
  bool _working = false;
  bool _disposed = false;
  bool _desiredRunning = false;
  DateTime? _nextRestart;
  bool? _lastConnected;
  @override
  LocalAgentState build() {
    final backend = ref.read(localBackendProvider);
    var config = backend.defaults;
    String? error;
    final saved = ref.read(prefsProvider).getString('local_agent');
    if (saved != null) {
      try {
        config = LocalAgentConfig.fromJson(
          jsonDecode(saved) as Map<String, dynamic>,
        );
      } on Object {
        error = '本机连接设置损坏，请重新填写';
      }
    }
    final timer = Timer.periodic(const Duration(seconds: 2), (_) => refresh());
    ref.onDispose(() {
      _disposed = true;
      timer.cancel();
    });
    return LocalAgentState(config: config, error: error);
  }

  String _message(Object error) => error is FormatException
      ? error.message.toString()
      : '无法操作本机 agent，请检查路径、版本和本机权限';

  Future<void> refresh() async {
    if (_working ||
        (state.config.controller.isEmpty && state.config.configFile.isEmpty)) {
      return;
    }
    _working = true;
    final old = state;
    final config = old.config;
    state = LocalAgentState(config: config, status: old.status, busy: true);
    try {
      final backend = ref.read(localBackendProvider);
      var status = await backend.status(config);
      if (status == null &&
          _desiredRunning &&
          (_nextRestart == null || DateTime.now().isAfter(_nextRestart!))) {
        await Future<void>.delayed(const Duration(seconds: 2));
        if (!_disposed && _desiredRunning) {
          await backend.start(config, '');
          status = await backend.status(config);
          _nextRestart = null;
        }
      }
      _recordConnection(status?.connected ?? false);
      if (!_disposed) state = LocalAgentState(config: config, status: status);
    } on Exception catch (e) {
      if (!_disposed) {
        if (_desiredRunning) _nextRestart = DateTime.now().add(const Duration(seconds: 10));
        state = LocalAgentState(
          config: config,
          status: old.status,
          error: _message(e),
        );
      }
    } finally {
      _working = false;
    }
  }

  void _recordConnection(bool connected) {
    final previous = _lastConnected;
    _lastConnected = connected;
    if (previous != null &&
        previous != connected &&
        ref.read(prefsProvider).getBool('notifications_enabled') != false) {
      unawaited(
        DesktopNotificationService.instance.agentConnectionChanged(connected),
      );
    }
  }

  Future<void> start(LocalAgentConfig config, String token) async {
    if (_working) return;
    if (state.status != null && config != state.config) {
      state = LocalAgentState(
        config: state.config,
        status: state.status,
        error: '请先停止当前 agent，再修改连接设置',
      );
      return;
    }
    _working = true;
    final old = state;
    final backend = ref.read(localBackendProvider);
    final prefs = ref.read(prefsProvider);
    state = LocalAgentState(config: config, status: old.status, busy: true);
    try {
      await prefs.setString('local_agent', jsonEncode(config.toJson()));
      await backend.start(config, token);
      _desiredRunning = true;
      _nextRestart = null;
      final status = await backend.status(config);
      if (status == null) throw const FormatException('agent 已退出，请检查接入令牌或证书');
      _recordConnection(status.connected);
      if (!_disposed) state = LocalAgentState(config: config, status: status);
    } on Exception catch (e) {
      if (!_disposed) {
        state = LocalAgentState(
          config: config,
          status: old.status,
          error: _message(e),
        );
      }
    } finally {
      _working = false;
    }
  }

  Future<void> stop() async {
    if (_working) return;
    _working = true;
    _desiredRunning = false;
    _nextRestart = null;
    final old = state;
    state = LocalAgentState(config: old.config, status: old.status, busy: true);
    try {
      await ref.read(localBackendProvider).stop(old.config);
      if (!_disposed) state = LocalAgentState(config: old.config);
    } on Exception catch (e) {
      if (!_disposed) {
        state = LocalAgentState(
          config: old.config,
          status: old.status,
          error: _message(e),
        );
      }
    } finally {
      _working = false;
    }
  }
}

final localAgentProvider =
    NotifierProvider<LocalAgentNotifier, LocalAgentState>(
      LocalAgentNotifier.new,
    );

final localLogsProvider = FutureProvider.autoDispose<List<String>>((ref) {
  final config = ref.watch(localAgentProvider.select((s) => s.config));
  return ref.watch(localBackendProvider).logs(config);
});

/// Allowlist-only diagnostic content, with a second redaction pass for old agents.
String diagnosticJson(
  LocalAgentStatus status,
  List<String> logs,
  DateTime capturedAt,
) {
  if (logs.length > 128 || logs.any((line) => utf8.encode(line).length > 512)) {
    throw const FormatException('诊断日志超出容量限制');
  }
  String redact(String line) {
    final lower = line.toLowerCase();
    return [
          'token',
          'password',
          'secret',
          'authorization',
          'private key',
          '/',
          r'\',
          ':',
        ].any(lower.contains)
        ? '[redacted]'
        : line;
  }

  return const JsonEncoder.withIndent('  ').convert({
    'schema_version': 1,
    'agent_version': status.agentVersion,
    'captured_at': capturedAt.toUtc().toIso8601String(),
    'status': {
      'pid': status.pid,
      'connected': status.connected,
      'revision': status.revision,
      'proxies': status.proxies,
    },
    'traffic': status.bytesIn == null
        ? null
        : {
            'bytes_in': status.bytesIn,
            'bytes_out': status.bytesOut,
            'connections': status.connections,
          },
    'logs': logs.map(redact).toList(growable: false),
    'limitations': '最近日志与状态分别读取；累计流量在进程重启后归零；端点和路径相关日志已隐藏。',
  });
}

final localTunnelsProvider =
    FutureProvider.autoDispose<List<Map<String, dynamic>>>(
      (ref) => ref
          .watch(localBackendProvider)
          .tunnels(ref.watch(localAgentProvider.select((s) => s.config))),
    );
