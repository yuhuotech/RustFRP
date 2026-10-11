import 'package:flutter/foundation.dart';

/// Transfer models. They mirror the controller's REST responses one-to-one (see
/// rustfrp-protocol/fixtures); view-specific composition lives in the features, not here.
///
/// Rules: immutable; tolerant of missing fields and unknown enum values (forward compatible);
/// times are UTC milliseconds.

enum Role {
  viewer,
  operator,
  admin,
  owner;

  /// Unknown roles get the lowest privilege.
  static Role fromWire(String? s) => switch (s) {
        'owner' => Role.owner,
        'admin' => Role.admin,
        'operator' => Role.operator,
        _ => Role.viewer,
      };

  bool atLeast(Role other) => index >= other.index;

  String get label => switch (this) {
        Role.owner => '所有者',
        Role.admin => '管理员',
        Role.operator => '运维',
        Role.viewer => '只读',
      };
}

int _int(Object? v, [int d = 0]) => v is num ? v.toInt() : d;
int? _intOrNull(Object? v) => v is num ? v.toInt() : null;
String _str(Object? v, [String d = '']) => v is String ? v : d;
Map<String, Object?> _map(Object? v) => v is Map<String, Object?> ? v : <String, Object?>{};
List<Object?> _list(Object? v) => v is List ? v : const [];

/// The server answered with something we cannot interpret.
class ProtocolException implements Exception {
  ProtocolException(this.message);
  final String message;
  @override
  String toString() => 'ProtocolException: $message';
}

@immutable
class Admin {
  const Admin({required this.id, required this.name, required this.role, this.disabledAt, this.createdAt = 0});
  final String id, name;
  final Role role;
  final int? disabledAt;
  final int createdAt;

  factory Admin.fromJson(Object? j) {
    final m = _map(j);
    if (m['id'] is! String) throw ProtocolException('admin without id');
    return Admin(
      id: _str(m['id']),
      name: _str(m['name']),
      role: Role.fromWire(m['role'] as String?),
      disabledAt: _intOrNull(m['disabled_at']),
      createdAt: _int(m['created_at']),
    );
  }

  @override
  bool operator ==(Object other) =>
      other is Admin && other.id == id && other.name == name && other.role == role && other.disabledAt == disabledAt && other.createdAt == createdAt;
  @override
  int get hashCode => Object.hash(id, name, role, disabledAt, createdAt);
}

@immutable
class ClientDevice {
  const ClientDevice({
    required this.id,
    required this.name,
    this.os = '',
    this.arch = '',
    this.version = '',
    this.revision = 0,
    this.createdAt = 0,
    this.lastSeenAt,
    this.disabledAt,
    this.online = false,
  });
  final String id, name, os, arch, version;
  final int revision, createdAt;
  final int? lastSeenAt, disabledAt;
  final bool online;

  bool get disabled => disabledAt != null;

  factory ClientDevice.fromJson(Object? j) {
    final m = _map(j);
    if (m['id'] is! String) throw ProtocolException('client without id');
    return ClientDevice(
      id: _str(m['id']),
      name: _str(m['name']),
      os: _str(m['os']),
      arch: _str(m['arch']),
      version: _str(m['version']),
      revision: _int(m['revision']),
      createdAt: _int(m['created_at']),
      lastSeenAt: _intOrNull(m['last_seen_at']),
      disabledAt: _intOrNull(m['disabled_at']),
      online: m['online'] == true,
    );
  }

  @override
  bool operator ==(Object other) =>
      other is ClientDevice &&
      other.id == id &&
      other.name == name &&
      other.os == os &&
      other.arch == arch &&
      other.version == version &&
      other.revision == revision &&
      other.createdAt == createdAt &&
      other.lastSeenAt == lastSeenAt &&
      other.disabledAt == disabledAt &&
      other.online == online;
  @override
  int get hashCode => Object.hash(id, name, os, arch, version, revision, createdAt, lastSeenAt, disabledAt, online);
}

enum TunnelState {
  pending,
  running,
  error,
  stopped,
  unknown;

  static TunnelState fromWire(String? s) => switch (s) {
        'pending' => TunnelState.pending,
        'running' => TunnelState.running,
        'error' => TunnelState.error,
        'stopped' => TunnelState.stopped,
        _ => TunnelState.unknown,
      };

  String get label => switch (this) {
        TunnelState.pending => '待启动',
        TunnelState.running => '运行中',
        TunnelState.error => '异常',
        TunnelState.stopped => '已停止',
        TunnelState.unknown => '未知',
      };
}

@immutable
class Tunnel {
  const Tunnel({
    required this.id,
    required this.clientId,
    required this.name,
    this.kind = 'tcp',
    this.localIp = '127.0.0.1',
    this.localPort = 0,
    this.remotePort = 0,
    this.enabled = true,
    this.createdAt = 0,
    this.updatedAt = 0,
    this.state = TunnelState.pending,
    this.statusPort = 0,
    this.statusError = '',
    this.customDomains = const [],
    this.subdomain = '',
    this.hostHeaderRewrite = '',
  });
  final String id, clientId, name, kind, localIp, statusError, subdomain, hostHeaderRewrite;
  final List<String> customDomains;

  bool get isWeb => kind == 'http' || kind == 'https';
  final int localPort, remotePort, statusPort, createdAt, updatedAt;
  final bool enabled;
  final TunnelState state;

  /// The port visitors use: the live one when running, otherwise the configured one (0 = auto).
  int get publicPort => statusPort != 0 ? statusPort : remotePort;

  factory Tunnel.fromJson(Object? j) {
    final m = _map(j);
    if (m['id'] is! String) throw ProtocolException('tunnel without id');
    return Tunnel(
      id: _str(m['id']),
      clientId: _str(m['client_id']),
      name: _str(m['name']),
      kind: _str(m['kind'], 'tcp'),
      localIp: _str(m['local_ip'], '127.0.0.1'),
      localPort: _int(m['local_port']),
      remotePort: _int(m['remote_port']),
      enabled: m['enabled'] != false,
      createdAt: _int(m['created_at']),
      updatedAt: _int(m['updated_at']),
      state: TunnelState.fromWire(m['state'] as String?),
      statusPort: _int(m['status_port']),
      statusError: _str(m['status_error']),
      customDomains: [for (final d in _list(m['custom_domains'])) if (d is String) d],
      subdomain: _str(m['subdomain']),
      hostHeaderRewrite: _str(m['host_header_rewrite']),
    );
  }

  @override
  bool operator ==(Object other) =>
      other is Tunnel &&
      other.id == id &&
      other.clientId == clientId &&
      other.name == name &&
      other.kind == kind &&
      other.localIp == localIp &&
      other.localPort == localPort &&
      other.remotePort == remotePort &&
      other.enabled == enabled &&
      other.createdAt == createdAt &&
      other.updatedAt == updatedAt &&
      other.state == state &&
      other.statusPort == statusPort &&
      other.statusError == statusError &&
      listEquals(other.customDomains, customDomains) &&
      other.subdomain == subdomain &&
      other.hostHeaderRewrite == hostHeaderRewrite;
  @override
  int get hashCode =>
      Object.hashAll([id, clientId, name, kind, localIp, localPort, remotePort, enabled, createdAt, updatedAt, state, statusPort, statusError, ...customDomains, subdomain, hostHeaderRewrite]);
}

@immutable
class EnrollToken {
  const EnrollToken({
    required this.id,
    required this.name,
    this.createdBy = '',
    this.createdAt = 0,
    this.expiresAt = 0,
    this.usedAt,
    this.clientId,
    this.revokedAt,
  });
  final String id, name, createdBy;
  final int createdAt, expiresAt;
  final int? usedAt, revokedAt;
  final String? clientId;

  /// pending / used / revoked / expired, evaluated at [nowMs].
  String status(int nowMs) {
    if (usedAt != null) return 'used';
    if (revokedAt != null) return 'revoked';
    if (expiresAt <= nowMs) return 'expired';
    return 'pending';
  }

  factory EnrollToken.fromJson(Object? j) {
    final m = _map(j);
    if (m['id'] is! String) throw ProtocolException('token without id');
    return EnrollToken(
      id: _str(m['id']),
      name: _str(m['name']),
      createdBy: _str(m['created_by']),
      createdAt: _int(m['created_at']),
      expiresAt: _int(m['expires_at']),
      usedAt: _intOrNull(m['used_at']),
      clientId: m['client_id'] as String?,
      revokedAt: _intOrNull(m['revoked_at']),
    );
  }
}

/// Response of creating an enroll token: the plaintext token appears exactly once.
@immutable
class EnrollResult {
  const EnrollResult({required this.id, required this.token, required this.expiresAt, required this.commands});
  final String id, token;
  final int expiresAt;
  final Map<String, String> commands;

  factory EnrollResult.fromJson(Object? j) {
    final m = _map(j);
    if (m['token'] is! String) throw ProtocolException('enroll result without token');
    return EnrollResult(
      id: _str(m['id']),
      token: _str(m['token']),
      expiresAt: _int(m['expires_at']),
      commands: {for (final e in _map(m['commands']).entries) if (e.value is String) e.key: e.value as String},
    );
  }
}

@immutable
class AuditEntry {
  const AuditEntry({
    required this.id,
    required this.ts,
    required this.actor,
    required this.action,
    this.targetKind = '',
    this.targetId = '',
    this.reason = '',
    this.result = '',
    this.ip = '',
  });
  final int id, ts;
  final String actor, action, targetKind, targetId, reason, result, ip;

  factory AuditEntry.fromJson(Object? j) {
    final m = _map(j);
    return AuditEntry(
      id: _int(m['id']),
      ts: _int(m['ts']),
      actor: _str(m['actor']),
      action: _str(m['action']),
      targetKind: _str(m['target_kind']),
      targetId: _str(m['target_id']),
      reason: _str(m['reason']),
      result: _str(m['result']),
      ip: _str(m['ip']),
    );
  }
}

@immutable
class Overview {
  const Overview({
    this.clients = 0,
    this.tunnels = 0,
    this.tunnelsRunning = 0,
    this.tokensPending = 0,
    this.clientsOnline = 0,
    this.version = '',
  });
  final int clients, tunnels, tunnelsRunning, tokensPending, clientsOnline;
  final String version;

  factory Overview.fromJson(Object? j) {
    final m = _map(j);
    final c = _map(m['counts']);
    return Overview(
      clients: _int(c['clients']),
      tunnels: _int(c['tunnels']),
      tunnelsRunning: _int(c['tunnels_running']),
      tokensPending: _int(c['tokens_pending']),
      clientsOnline: _int(m['clients_online']),
      version: _str(m['version']),
    );
  }
}

@immutable
class NodeInfo {
  const NodeInfo({
    this.name = 'builtin',
    this.publicHost = '',
    this.edgePort = 7000,
    this.allowPorts = const [],
    this.version = '',
    this.clientsOnline = 0,
    this.vhostHttpPort,
    this.vhostHttpsPort,
    this.subdomainHost = '',
  });
  final String name, publicHost, version, subdomainHost;
  final int edgePort, clientsOnline;
  final int? vhostHttpPort, vhostHttpsPort;

  /// Inclusive `[lo, hi]` ranges; empty = unrestricted.
  final List<(int, int)> allowPorts;

  String get allowPortsLabel =>
      allowPorts.isEmpty ? '不限制' : allowPorts.map((r) => r.$1 == r.$2 ? '${r.$1}' : '${r.$1}-${r.$2}').join(', ');

  factory NodeInfo.fromJson(Object? j) {
    final m = _map(j);
    final ranges = <(int, int)>[];
    for (final r in _list(m['allow_ports'])) {
      final l = _list(r);
      if (l.length == 2) ranges.add((_int(l[0]), _int(l[1])));
    }
    return NodeInfo(
      name: _str(m['name'], 'builtin'),
      publicHost: _str(m['public_host']),
      edgePort: _int(m['edge_port'], 7000),
      allowPorts: ranges,
      version: _str(m['version']),
      clientsOnline: _int(m['clients_online']),
      vhostHttpPort: _intOrNull(m['vhost_http_port']),
      vhostHttpsPort: _intOrNull(m['vhost_https_port']),
      subdomainHost: _str(m['subdomain_host']),
    );
  }
}

/// An error reported by the controller (or by the network layer, with [status] 0).
class ApiException implements Exception {
  ApiException(this.status, this.code, this.message);
  final int status;
  final String code, message;

  bool get unauthorized => status == 401;
  @override
  String toString() => message;
}
