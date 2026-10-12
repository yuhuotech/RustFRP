import 'dart:async';
import 'dart:convert';

import 'package:http/http.dart' as http;

import 'models.dart';

/// REST client for the controller's `/api/v1`.
///
/// Auth model (matches the server): an HttpOnly session cookie plus an `X-CSRF-Token` header on writes.
/// In a browser the cookie is handled by the browser; on desktop [ApiClient] keeps its own tiny cookie jar.
class ApiClient {
  ApiClient({
    required this.baseUrl,
    http.Client? client,
    this.timeout = const Duration(seconds: 15),
  }) : _http = client ?? http.Client();

  /// `''` = same origin (web); otherwise `https://host:port` without a trailing slash.
  final String baseUrl;
  final Duration timeout;
  final http.Client _http;

  String? _cookie;
  String? csrf;
  int _controllerLogCursor = 0;
  final List<String> _controllerLogCache = [];

  /// Called whenever the server says the session is no longer valid.
  void Function()? onUnauthorized;

  bool get hasSession => csrf != null;

  Uri _uri(String path, [Map<String, String>? query]) {
    final base = Uri.parse('$baseUrl/api/v1$path');
    return query == null || query.isEmpty
        ? base
        : base.replace(queryParameters: query);
  }

  Map<String, String> _headers({bool write = false, bool login = false}) => {
    'accept': 'application/json',
    if (write) 'content-type': 'application/json',
    'cookie': ?_cookie,
    'x-csrf-token': ?(write ? csrf : null),
    if (login) 'x-rustfrp-admin': '1',
  };

  Future<Object?> _send(
    String method,
    String path, {
    Map<String, String>? query,
    Object? body,
    bool login = false,
  }) async {
    final req = http.Request(method, _uri(path, query));
    req.headers.addAll(_headers(write: method != 'GET', login: login));
    if (body != null) req.body = jsonEncode(body);
    http.Response res;
    try {
      res = await http.Response.fromStream(
        await _http.send(req).timeout(timeout),
      );
    } on TimeoutException {
      throw ApiException(0, 'timeout', '连接服务器超时');
    } on Exception catch (e) {
      throw ApiException(0, 'network', '无法连接服务器：$e');
    }
    final setCookie = res.headers['set-cookie'];
    if (setCookie != null && setCookie.isNotEmpty) {
      final pair = setCookie.split(';').first.trim();
      // A cleared cookie (logout) has an empty value.
      _cookie = pair.endsWith('=') ? null : pair;
    }
    Object? json;
    if (res.body.isNotEmpty) {
      try {
        json = jsonDecode(utf8.decode(res.bodyBytes));
      } on FormatException {
        json = null;
      }
    }
    if (res.statusCode >= 200 && res.statusCode < 300) return json;
    final err = json is Map && json['error'] is Map
        ? json['error'] as Map
        : const {};
    final ex = ApiException(
      res.statusCode,
      (err['code'] as String?) ?? 'http_${res.statusCode}',
      (err['message'] as String?) ?? '请求失败（${res.statusCode}）',
    );
    if (ex.unauthorized && !login) {
      _cookie = null;
      csrf = null;
      onUnauthorized?.call();
    }
    throw ex;
  }

  List<T> _items<T>(Object? j, T Function(Object?) f) {
    if (j is! List) throw ProtocolException('expected a list');
    return [for (final e in j) f(e)];
  }

  // ───────── auth ─────────

  Future<Admin> login(String name, String password) async {
    final j = await _send(
      'POST',
      '/auth/login',
      body: {'name': name, 'password': password},
      login: true,
    );
    final m = j is Map ? j : const {};
    csrf = m['csrf'] as String?;
    return Admin.fromJson(m['admin']);
  }

  /// Restores a session (browser cookie or desktop jar). Returns null when not logged in.
  Future<Admin?> me() async {
    try {
      final j = await _send('GET', '/auth/me');
      final m = j is Map ? j : const {};
      csrf = m['csrf'] as String?;
      return Admin.fromJson(m['admin']);
    } on ApiException catch (e) {
      if (e.unauthorized) return null;
      rethrow;
    }
  }

  Future<void> logout() async {
    try {
      await _send('POST', '/auth/logout', body: const {});
    } finally {
      _cookie = null;
      csrf = null;
    }
  }

  // ───────── reads ─────────

  Future<TrafficHistory> traffic({
    int hours = 24,
    String? tunnelId,
    String? clientId,
  }) async => TrafficHistory.fromJson(
    await _send(
      'GET',
      '/traffic',
      query: {
        'hours': '$hours',
        'tunnel_id': ?tunnelId,
        'client_id': ?clientId,
      },
    ),
  );

  Future<Overview> overview() async =>
      Overview.fromJson(await _send('GET', '/overview'));
  Future<NodeInfo> node() async =>
      NodeInfo.fromJson(await _send('GET', '/node'));
  Future<List<NodeInfo>> nodes() async =>
      _items(await _send('GET', '/nodes'), NodeInfo.fromJson);
  Future<List<ClientDevice>> clients() async =>
      _items(await _send('GET', '/clients'), ClientDevice.fromJson);
  Future<List<Tunnel>> tunnels({String? clientId}) async => _items(
    await _send('GET', '/tunnels', query: {'client_id': ?clientId}),
    Tunnel.fromJson,
  );
  Future<List<Visitor>> visitors({String? clientId}) async => _items(
    await _send('GET', '/visitors', query: {'client_id': ?clientId}),
    Visitor.fromJson,
  );

  Future<Visitor> createVisitor({
    required String clientId,
    required String name,
    required String tunnelId,
    String bindAddr = '127.0.0.1',
    int bindPort = 0,
    bool enabled = true,
    bool useEncryption = false,
    bool useCompression = false,
    required String reason,
  }) async => Visitor.fromJson(
    await _send(
      'POST',
      '/visitors',
      body: {
        'client_id': clientId,
        'name': name,
        'tunnel_id': tunnelId,
        'bind_addr': bindAddr,
        'bind_port': bindPort,
        'enabled': enabled,
        'use_encryption': useEncryption,
        'use_compression': useCompression,
        'reason': reason,
      },
    ),
  );

  Future<Visitor> patchVisitor(
    String id, {
    String? name,
    String? tunnelId,
    String? bindAddr,
    int? bindPort,
    bool? enabled,
    bool? useEncryption,
    bool? useCompression,
    required String reason,
  }) async => Visitor.fromJson(
    await _send(
      'PATCH',
      '/visitors/$id',
      body: {
        'name': ?name,
        'tunnel_id': ?tunnelId,
        'bind_addr': ?bindAddr,
        'bind_port': ?bindPort,
        'enabled': ?enabled,
        'use_encryption': ?useEncryption,
        'use_compression': ?useCompression,
        'reason': reason,
      },
    ),
  );

  Future<void> deleteVisitor(String id, {required String reason}) async =>
      _send('DELETE', '/visitors/$id', query: {'reason': reason});

  Future<List<String>> controllerLogs() async {
    final raw = await _send(
      'GET',
      '/logs',
      query: {'after': '$_controllerLogCursor'},
    );
    if (raw is! Map<String, dynamic> || raw['items'] is! List) {
      throw ProtocolException('controller log response is invalid');
    }
    final items = (raw['items'] as List).whereType<String>().toList();
    if (items.length > 128 || items.length != (raw['items'] as List).length) {
      throw ProtocolException('controller log page exceeds bounds');
    }
    if (raw['gap'] == true) _controllerLogCache.clear();
    _controllerLogCache.addAll(items);
    if (_controllerLogCache.length > 128) {
      _controllerLogCache.removeRange(0, _controllerLogCache.length - 128);
    }
    final cursor = raw['cursor'];
    if (cursor is int && cursor >= 0) _controllerLogCursor = cursor;
    return List.unmodifiable(_controllerLogCache);
  }

  Future<List<Map<String, dynamic>>> apiTokens() async =>
      (await _send('GET', '/api-tokens') as List).cast<Map<String, dynamic>>();
  Future<Map<String, dynamic>> createApiToken(
    String name,
    String scope,
    int days,
    String reason,
  ) async => (await _send(
    'POST',
    '/api-tokens',
    body: {'name': name, 'scope': scope, 'days': days, 'reason': reason},
  ) as Map).cast<String, dynamic>();
  Future<void> revokeApiToken(String id, String reason) async {
    await _send('POST', '/api-tokens/$id/revoke', body: {'reason': reason});
  }

  Future<List<EnrollToken>> enrollTokens() async =>
      _items(await _send('GET', '/enroll-tokens'), EnrollToken.fromJson);
  Future<List<AuditEntry>> audit({int limit = 100, int? before}) async =>
      _items(
        await _send(
          'GET',
          '/audit',
          query: {
            'limit': '$limit',
            'before': ?(before == null ? null : '$before'),
          },
        ),
        AuditEntry.fromJson,
      );
  Future<List<Admin>> admins() async =>
      _items(await _send('GET', '/admins'), Admin.fromJson);

  // ───────── writes (every write carries a reason; the server audits it) ─────────

  Future<Admin> createAdmin({
    required String name,
    required String password,
    required Role role,
    required String reason,
  }) async => Admin.fromJson(
    await _send(
      'POST',
      '/admins',
      body: {
        'name': name,
        'password': password,
        'role': role.name,
        'reason': reason,
      },
    ),
  );

  Future<Admin> patchAdmin(
    String id, {
    Role? role,
    bool? disabled,
    String? password,
    required String reason,
  }) async => Admin.fromJson(
    await _send(
      'PATCH',
      '/admins/$id',
      body: {
        'role': ?role?.name,
        'disabled': ?disabled,
        'password': ?password,
        'reason': reason,
      },
    ),
  );

  Future<ClientDevice> patchClient(
    String id, {
    String? name,
    bool? disabled,
    List<String>? tags,
    int? maxConnections,
    required String reason,
  }) async => ClientDevice.fromJson(
    await _send(
      'PATCH',
      '/clients/$id',
      body: {
        'name': ?name,
        'disabled': ?disabled,
        'tags': ?tags,
        'max_connections': ?maxConnections,
        'reason': reason,
      },
    ),
  );

  Future<void> restartClient(String id, {required String reason}) async =>
      _send('POST', '/clients/$id/restart', body: {'reason': reason});

  Future<void> migrateClient(
    String id,
    String nodeId, {
    required String reason,
  }) async => _send(
    'POST',
    '/clients/$id/migrate',
    body: {'node_id': nodeId, 'reason': reason},
  );

  Future<void> updateClientAgent(
    String id,
    String version, {
    required String reason,
  }) async => _send(
    'POST',
    '/clients/$id/update',
    body: {'version': version, 'reason': reason},
  );

  Future<void> kickClient(String id, {required String reason}) async =>
      _send('POST', '/clients/$id/kick', body: {'reason': reason});

  Future<void> deleteClient(String id, {required String reason}) async =>
      _send('DELETE', '/clients/$id', query: {'reason': reason});

  Future<void> batchTunnels(
    List<String> ids,
    bool enabled,
    String reason,
  ) async {
    await _send(
      'POST',
      '/tunnels/batch',
      body: {'ids': ids, 'enabled': enabled, 'reason': reason},
    );
  }

  Future<Tunnel> createTunnel({
    required String clientId,
    required String name,
    required int localPort,
    String localIp = '127.0.0.1',
    int remotePort = 0,
    bool enabled = true,
    String kind = 'tcp',
    List<String> customDomains = const [],
    String subdomain = '',
    String hostHeaderRewrite = '',
    List<String> locations = const [],
    String httpUser = '',
    String httpPassword = '',
    String routeByHttpUser = '',
    Map<String, String> requestHeaders = const {},
    Map<String, String> responseHeaders = const {},
    int proxyProtocol = 0,
    int bandwidthBps = 0,
    bool bandwidthServer = false,
    int maxConnections = 0,
    String multiplexer = '',
    bool useEncryption = false,
    bool useCompression = false,
    String lbGroup = '',
    String lbGroupKey = '',
    HealthCheck? healthCheck,
    Map<String, dynamic>? plugin,
    required String reason,
  }) async => Tunnel.fromJson(
    await _send(
      'POST',
      '/tunnels',
      body: {
        'client_id': clientId,
        'name': name,
        'kind': kind,
        'custom_domains': customDomains,
        'subdomain': subdomain,
        'host_header_rewrite': hostHeaderRewrite,
        'locations': locations,
        'http_user': httpUser,
        'http_password': httpPassword,
        'route_by_http_user': routeByHttpUser,
        'request_headers': requestHeaders,
        'response_headers': responseHeaders,
        'proxy_protocol': proxyProtocol,
        'bandwidth_bps': bandwidthBps,
        'bandwidth_server': bandwidthServer,
        'max_connections': maxConnections,
        'multiplexer': multiplexer,
        'use_encryption': useEncryption,
        'use_compression': useCompression,
        'lb_group': lbGroup,
        'lb_group_key': lbGroupKey,
        'health_check': ?healthCheck?.toJson(),
        'plugin': ?plugin,
        'local_ip': localIp,
        'local_port': localPort,
        'remote_port': remotePort,
        'enabled': enabled,
        'reason': reason,
      },
    ),
  );

  Future<Tunnel> patchTunnel(
    String id, {
    String? name,
    String? localIp,
    int? localPort,
    int? remotePort,
    bool? enabled,
    List<String>? customDomains,
    String? subdomain,
    String? hostHeaderRewrite,
    List<String>? locations,
    String? httpUser,
    String? httpPassword,
    String? routeByHttpUser,
    Map<String, String>? requestHeaders,
    Map<String, String>? responseHeaders,
    int? proxyProtocol,
    int? bandwidthBps,
    bool? bandwidthServer,
    int? maxConnections,
    String? multiplexer,
    bool? useEncryption,
    bool? useCompression,
    String? lbGroup,
    String? lbGroupKey,
    HealthCheck? healthCheck,
    Map<String, dynamic>? plugin,
    required String reason,
  }) async => Tunnel.fromJson(
    await _send(
      'PATCH',
      '/tunnels/$id',
      body: {
        'name': ?name,
        'local_ip': ?localIp,
        'local_port': ?localPort,
        'remote_port': ?remotePort,
        'enabled': ?enabled,
        'custom_domains': ?customDomains,
        'subdomain': ?subdomain,
        'host_header_rewrite': ?hostHeaderRewrite,
        'locations': ?locations,
        'http_user': ?httpUser,
        'http_password': ?httpPassword,
        'route_by_http_user': ?routeByHttpUser,
        'request_headers': ?requestHeaders,
        'response_headers': ?responseHeaders,
        'proxy_protocol': ?proxyProtocol,
        'bandwidth_bps': ?bandwidthBps,
        'bandwidth_server': ?bandwidthServer,
        'max_connections': ?maxConnections,
        'multiplexer': ?multiplexer,
        'use_encryption': ?useEncryption,
        'use_compression': ?useCompression,
        'lb_group': ?lbGroup,
        'lb_group_key': ?lbGroupKey,
        'health_check': ?healthCheck?.toJson(),
        'plugin': ?plugin,
        'reason': reason,
      },
    ),
  );

  Future<void> deleteTunnel(String id, {required String reason}) async =>
      _send('DELETE', '/tunnels/$id', query: {'reason': reason});

  Future<Map<String, dynamic>> previewConfigImport(
    String clientId,
    String text,
  ) async => (await _send(
    'POST',
    '/clients/$clientId/config/preview',
    body: {'text': text},
  )) as Map<String, dynamic>;

  Future<void> importConfig(
    String clientId, {
    required String text,
    required String digest,
    required String reason,
  }) async {
    await _send(
      'POST',
      '/clients/$clientId/config/import',
      body: {
        'text': text,
        'digest': digest,
        'accept_warnings': true,
        'reason': reason,
      },
    );
  }

  Future<String> exportConfig(String clientId) async =>
      ((await _send('GET', '/clients/$clientId/config/export')
              as Map<String, dynamic>)['text']
          as String);

  /// [kind]：`client`（默认）或 `node`；[nodeId]：设备令牌指定由哪个外置节点服务。
  Future<EnrollResult> createEnrollToken({
    required String name,
    int ttlMinutes = 60,
    String kind = 'client',
    String nodeId = '',
    required String reason,
  }) async => EnrollResult.fromJson(
    await _send(
      'POST',
      '/enroll-tokens',
      body: {
        'name': name,
        'ttl_minutes': ttlMinutes,
        'kind': kind,
        'node_id': nodeId,
        'reason': reason,
      },
    ),
  );

  Future<NodeInfo> patchNode(
    String id, {
    String? name,
    bool? disabled,
    required String reason,
  }) async => NodeInfo.fromJson(
    await _send(
      'PATCH',
      '/nodes/$id',
      body: {'name': ?name, 'disabled': ?disabled, 'reason': reason},
    ),
  );

  Future<void> deleteNode(String id, {required String reason}) async =>
      _send('DELETE', '/nodes/$id', query: {'reason': reason});

  Future<void> revokeEnrollToken(String id, {required String reason}) async =>
      _send('DELETE', '/enroll-tokens/$id', query: {'reason': reason});

  void close() => _http.close();
}
