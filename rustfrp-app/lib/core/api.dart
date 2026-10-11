import 'dart:async';
import 'dart:convert';

import 'package:http/http.dart' as http;

import 'models.dart';

/// REST client for the controller's `/api/v1`.
///
/// Auth model (matches the server): an HttpOnly session cookie plus an `X-CSRF-Token` header on writes.
/// In a browser the cookie is handled by the browser; on desktop [ApiClient] keeps its own tiny cookie jar.
class ApiClient {
  ApiClient({required this.baseUrl, http.Client? client, this.timeout = const Duration(seconds: 15)})
      : _http = client ?? http.Client();

  /// `''` = same origin (web); otherwise `https://host:port` without a trailing slash.
  final String baseUrl;
  final Duration timeout;
  final http.Client _http;

  String? _cookie;
  String? csrf;

  /// Called whenever the server says the session is no longer valid.
  void Function()? onUnauthorized;

  bool get hasSession => csrf != null;

  Uri _uri(String path, [Map<String, String>? query]) {
    final base = Uri.parse('$baseUrl/api/v1$path');
    return query == null || query.isEmpty ? base : base.replace(queryParameters: query);
  }

  Map<String, String> _headers({bool write = false, bool login = false}) => {
        'accept': 'application/json',
        if (write) 'content-type': 'application/json',
        'cookie': ?_cookie,
        'x-csrf-token': ?(write ? csrf : null),
        if (login) 'x-rustfrp-admin': '1',
      };

  Future<Object?> _send(String method, String path,
      {Map<String, String>? query, Object? body, bool login = false}) async {
    final req = http.Request(method, _uri(path, query));
    req.headers.addAll(_headers(write: method != 'GET', login: login));
    if (body != null) req.body = jsonEncode(body);
    http.Response res;
    try {
      res = await http.Response.fromStream(await _http.send(req).timeout(timeout));
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
    final err = json is Map && json['error'] is Map ? json['error'] as Map : const {};
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
    final j = await _send('POST', '/auth/login', body: {'name': name, 'password': password}, login: true);
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

  Future<Overview> overview() async => Overview.fromJson(await _send('GET', '/overview'));
  Future<NodeInfo> node() async => NodeInfo.fromJson(await _send('GET', '/node'));
  Future<List<ClientDevice>> clients() async => _items(await _send('GET', '/clients'), ClientDevice.fromJson);
  Future<List<Tunnel>> tunnels({String? clientId}) async =>
      _items(await _send('GET', '/tunnels', query: {'client_id': ?clientId}), Tunnel.fromJson);
  Future<List<EnrollToken>> enrollTokens() async => _items(await _send('GET', '/enroll-tokens'), EnrollToken.fromJson);
  Future<List<AuditEntry>> audit({int limit = 100, int? before}) async => _items(
      await _send('GET', '/audit', query: {'limit': '$limit', 'before': ?(before == null ? null : '$before')}),
      AuditEntry.fromJson);
  Future<List<Admin>> admins() async => _items(await _send('GET', '/admins'), Admin.fromJson);

  // ───────── writes (every write carries a reason; the server audits it) ─────────

  Future<ClientDevice> patchClient(String id, {String? name, bool? disabled, required String reason}) async =>
      ClientDevice.fromJson(await _send('PATCH', '/clients/$id', body: {
        'name': ?name,
        'disabled': ?disabled,
        'reason': reason,
      }));

  Future<void> kickClient(String id, {required String reason}) async =>
      _send('POST', '/clients/$id/kick', body: {'reason': reason});

  Future<void> deleteClient(String id, {required String reason}) async =>
      _send('DELETE', '/clients/$id', query: {'reason': reason});

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
    required String reason,
  }) async =>
      Tunnel.fromJson(await _send('POST', '/tunnels', body: {
        'client_id': clientId,
        'name': name,
        'kind': kind,
        'custom_domains': customDomains,
        'subdomain': subdomain,
        'host_header_rewrite': hostHeaderRewrite,
        'local_ip': localIp,
        'local_port': localPort,
        'remote_port': remotePort,
        'enabled': enabled,
        'reason': reason,
      }));

  Future<Tunnel> patchTunnel(String id,
          {String? name,
          String? localIp,
          int? localPort,
          int? remotePort,
          bool? enabled,
          List<String>? customDomains,
          String? subdomain,
          String? hostHeaderRewrite,
          required String reason}) async =>
      Tunnel.fromJson(await _send('PATCH', '/tunnels/$id', body: {
        'name': ?name,
        'local_ip': ?localIp,
        'local_port': ?localPort,
        'remote_port': ?remotePort,
        'enabled': ?enabled,
        'custom_domains': ?customDomains,
        'subdomain': ?subdomain,
        'host_header_rewrite': ?hostHeaderRewrite,
        'reason': reason,
      }));

  Future<void> deleteTunnel(String id, {required String reason}) async =>
      _send('DELETE', '/tunnels/$id', query: {'reason': reason});

  Future<EnrollResult> createEnrollToken({required String name, int ttlMinutes = 60, required String reason}) async =>
      EnrollResult.fromJson(await _send('POST', '/enroll-tokens',
          body: {'name': name, 'ttl_minutes': ttlMinutes, 'reason': reason}));

  Future<void> revokeEnrollToken(String id, {required String reason}) async =>
      _send('DELETE', '/enroll-tokens/$id', query: {'reason': reason});

  void close() => _http.close();
}
