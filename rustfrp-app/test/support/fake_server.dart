import 'dart:convert';

import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:rustfrp_app/core/api.dart';

/// In-memory stand-in for the controller's REST API (same JSON shapes and error codes as rustfrp-server).
class FakeServer {
  FakeServer({this.role = 'owner'});

  String role;
  final String adminName = 'root';
  final String adminPassword = 'correct horse battery';
  bool loggedIn = false; // whether the (single) session cookie is valid
  bool failNext = false;
  bool offline = false;
  final List<Map<String, Object?>> traffic = [];
  int trafficWindowEnd = 3600000;
  bool failTraffic = false;
  List<String> controllerLogs = [];
  int _seq = 0;
  final List<http.Request> requests = [];

  final List<Map<String, Object?>> clients = [];
  final List<Map<String, Object?>> tunnels = [];
  final List<Map<String, Object?>> tokens = [];
  final List<Map<String, Object?>> apiTokens = [];
  final List<Map<String, Object?>> audit = [];

  /// 内置节点是否启用（`/node` 与 `/nodes` 据此返回）。
  bool builtinEnabled = true;

  /// 收到重启请求的设备。
  final List<String> restarts = [];

  /// 外置节点（`/nodes` 里排在内置节点之后）。
  final List<Map<String, Object?>> extraNodes = [];

  /// 隧道 id → Basic 认证密码（服务器永不返回，这里仅用于断言客户端发了什么）。
  final Map<String, String> httpPasswords = {};
  final List<Map<String, Object?>> extraAdmins = [
    {
      'id': 'a2',
      'name': 'ops',
      'role': 'viewer',
      'disabled_at': 5,
      'created_at': 2,
    },
  ];
  final Set<String> online = {};
  String publicHost = 'frp.example.com';
  String publicUrl = 'https://frp.example.com';

  late final MockClient client = MockClient(_handle);

  ApiClient api([String base = '']) => ApiClient(baseUrl: base, client: client);

  String _id() => 'id${++_seq}';

  Map<String, Object?> addClient(
    String name, {
    String os = 'linux',
    bool isOnline = false,
    bool disabled = false,
  }) {
    final c = {
      'id': _id(),
      'name': name,
      'os': os,
      'arch': 'x86_64',
      'version': '0.1.0',
      'revision': 1,
      'created_at': 1700000000000,
      'last_seen_at': 1700000100000,
      'disabled_at': disabled ? 1700000200000 : null,
      'last_ip': isOnline ? '203.0.113.7' : '',
    };
    clients.add(c);
    if (isOnline) online.add(c['id']! as String);
    return c;
  }

  Map<String, Object?> addNode(
    String name, {
    bool isOnline = true,
    bool disabled = false,
    int clients = 0,
    String host = 'edge.example.com',
  }) {
    final n = {
      'id': _id(),
      'name': name,
      'builtin': false,
      'public_host': host,
      'edge_port': 7000,
      'tls_fingerprint': 'f' * 64,
      'vhost_http': 80,
      'vhost_https': 443,
      'subdomain_host': 'n.example.com',
      'allow_ports': [
        [20000, 20100],
      ],
      'version': '0.2.0',
      'os': 'linux',
      'created_at': 1700000000000,
      'last_seen_at': 1700000100000,
      'disabled_at': disabled ? 1700000200000 : null,
      'online': isOnline,
      'clients': clients,
    };
    extraNodes.add(n);
    return n;
  }

  final visitors = <Map<String, Object?>>[];

  Map<String, Object?> addVisitor(
    String clientId,
    String name,
    String tunnelId, {
    String addr = '127.0.0.1',
    int port = 0,
    bool enabled = true,
  }) {
    final v = <String, Object?>{
      'id': _id(),
      'client_id': clientId,
      'name': name,
      'tunnel_id': tunnelId,
      'bind_addr': addr,
      'bind_port': port,
      'enabled': enabled,
      'created_at': 1,
      'updated_at': 1,
    };
    visitors.add(v);
    return v;
  }

  Map<String, Object?> addTunnel(
    String clientId,
    String name, {
    int local = 22,
    int remote = 6000,
    String state = 'running',
    bool enabled = true,
    String error = '',
  }) {
    final t = {
      'id': _id(),
      'client_id': clientId,
      'name': name,
      'kind': 'tcp',
      'local_ip': '127.0.0.1',
      'local_port': local,
      'remote_port': remote,
      'enabled': enabled,
      'created_at': 1,
      'updated_at': 1,
      'state': state,
      'status_port': state == 'running' ? remote : 0,
      'status_error': error,
      'custom_domains': <String>[],
      'subdomain': '',
      'host_header_rewrite': '',
    };
    tunnels.add(t);
    return t;
  }

  http.Response _json(
    Object? body, [
    int status = 200,
    Map<String, String> headers = const {},
  ]) => http.Response(
    jsonEncode(body),
    status,
    headers: {'content-type': 'application/json', ...headers},
  );

  http.Response _err(int status, String code, String message) => _json({
    'error': {'code': code, 'message': message},
  }, status);

  int _rank(String r) =>
      const ['viewer', 'operator', 'admin', 'owner'].indexOf(r);

  Map<String, Object?> get _admin => {
    'id': 'a1',
    'name': adminName,
    'role': role,
    'disabled_at': null,
    'created_at': 1,
  };

  Future<http.Response> _handle(http.Request r) async {
    requests.add(r);
    if (offline) throw http.ClientException('connection refused');
    if (failNext) {
      failNext = false;
      return _err(500, 'internal', 'internal error');
    }
    final path = r.url.path.replaceFirst('/api/v1', '');
    final body = r.body.isEmpty
        ? <String, Object?>{}
        : jsonDecode(r.body) as Map<String, Object?>;

    if (path == '/auth/login') {
      if (r.headers['x-rustfrp-admin'] == null) {
        return _err(403, 'forbidden', 'login must come from the panel');
      }
      if (body['name'] == adminName && body['password'] == adminPassword) {
        loggedIn = true;
        return _json(
          {'admin': _admin, 'csrf': 'csrf-token'},
          200,
          {'set-cookie': 'rfrp_session=tok123; HttpOnly; Path=/'},
        );
      }
      return _err(401, 'invalid_credentials', 'invalid user name or password');
    }
    final cookieOk =
        loggedIn &&
        (r.headers['cookie'] ?? 'rfrp_session=tok123').contains(
          'rfrp_session=tok123',
        );
    if (!cookieOk) return _err(401, 'unauthorized', 'login required');
    if (path == '/auth/me') {
      return _json({'admin': _admin, 'csrf': 'csrf-token'});
    }
    if (path == '/auth/logout') {
      loggedIn = false;
      return _json(
        {'ok': true},
        200,
        {'set-cookie': 'rfrp_session=; Max-Age=0'},
      );
    }
    final write = r.method != 'GET';
    if (write && r.headers['x-csrf-token'] != 'csrf-token') {
      return _err(403, 'forbidden', 'missing or invalid CSRF token');
    }

    String reason() =>
        (body['reason'] as String?) ?? r.url.queryParameters['reason'] ?? '';
    http.Response? guard(String min) {
      if (_rank(role) < _rank(min)) {
        return _err(403, 'forbidden', 'insufficient permissions');
      }
      if (write && reason().trim().isEmpty) {
        return _err(
          400,
          'bad_request',
          'reason is required (1-200 characters)',
        );
      }
      return null;
    }

    final seg = path.split('/').where((s) => s.isNotEmpty).toList();
    switch ((r.method, seg)) {
      case ('GET', ['logs']):
        final denied = guard('admin');
        if (denied != null) return denied;
        return _json({
          'items': controllerLogs,
          'capacity': 128,
          'line_bytes': 512,
        });
      case ('GET', ['api-tokens']):
        return _json(apiTokens);
      case ('POST', ['api-tokens']):
        final denied = guard('owner');
        if (denied != null) return denied;
        final info = {
          'id': _id(),
          'name': body['name'],
          'scope': body['scope'],
          'created_at': 1,
          'expires_at': 2000000000000,
          'revoked_at': null,
        };
        apiTokens.add(info);
        return _json({'info': info, 'token': 'rfrp_automation_test'});
      case ('POST', ['api-tokens', final id, 'revoke']):
        final denied = guard('owner');
        if (denied != null) return denied;
        apiTokens.firstWhere((t) => t['id'] == id)['revoked_at'] = 1;
        return _json({'ok': true});
      case ('POST', ['tunnels', 'batch']):
        if (role == 'viewer') return _err(403, 'forbidden', 'forbidden');
        final ids = body['ids'] as List;
        if (ids.any((id) => !tunnels.any((t) => t['id'] == id))) {
          return _err(404, 'not_found', 'not found');
        }
        for (final t in tunnels.where((t) => ids.contains(t['id']))) {
          t['enabled'] = body['enabled'];
        }
        return _json({'updated': ids.length});
      case ('GET', ['traffic']):
        if (failTraffic) {
          return _json({
            'error': {'code': 'unavailable', 'message': 'traffic unavailable'},
          }, 503);
        }
        return _json({
          'range_hours': int.parse(r.url.queryParameters['hours'] ?? '24'),
          'window_end': trafficWindowEnd,
          'bucket_ms': 3600000,
          'sampled_only': true,
          'items': traffic,
        });
      case ('GET', ['overview']):
        return _json({
          'counts': {
            'clients': clients.length,
            'tunnels': tunnels.length,
            'tunnels_running': tunnels
                .where((t) => t['state'] == 'running')
                .length,
            'tokens_pending': tokens
                .where((t) => t['used_at'] == null && t['revoked_at'] == null)
                .length,
          },
          'clients_online': online.length,
          'traffic_24h': {'bytes_in': 2048, 'bytes_out': 1024},
          'process': {
            'uptime_seconds': 90000,
            'rss_bytes': 5 * 1024 * 1024,
            'cpu_seconds': 12.34,
          },
          'version': '0.1.0',
        });
      case ('GET', ['node']):
        return _json({
          'name': 'builtin',
          'enabled': builtinEnabled,
          'public_host': publicHost,
          'edge_port': 7000,
          'allow_ports': [
            [6000, 6100],
            [7000, 7000],
          ],
          'vhost_http_port': 8080,
          'vhost_https_port': null,
          'subdomain_host': 't.example.com',
          'version': '0.1.0',
          'clients_online': online.length,
        });
      case ('GET', ['nodes']):
        return _json([
          if (builtinEnabled)
            {
              'id': '',
              'name': 'builtin',
              'builtin': true,
              'public_host': publicHost,
              'edge_port': 7000,
              'tls_fingerprint': '',
              'vhost_http': 8080,
              'vhost_https': null,
              'subdomain_host': 't.example.com',
              'allow_ports': [
                [6000, 6100],
              ],
              'version': '0.1.0',
              'os': 'linux',
              'created_at': 0,
              'last_seen_at': null,
              'disabled_at': null,
              'online': true,
              'clients': clients
                  .where((c) => (c['node_id'] ?? '') == '')
                  .length,
            },
          ...extraNodes,
        ]);
      case ('PATCH', ['nodes', final id]):
        final e = guard('admin');
        if (e != null) return e;
        final n = extraNodes.where((x) => x['id'] == id).firstOrNull;
        if (n == null) return _err(404, 'not_found', 'not found');
        if (body.containsKey('name')) n['name'] = body['name'];
        if (body.containsKey('disabled')) {
          n['disabled_at'] = body['disabled'] == true ? 1700000300000 : null;
        }
        return _json(n);
      case ('DELETE', ['nodes', final id]):
        final e = guard('admin');
        if (e != null) return e;
        final n = extraNodes.where((x) => x['id'] == id).firstOrNull;
        if (n == null) return _err(404, 'not_found', 'not found');
        if ((n['clients'] as int? ?? 0) > 0) {
          return _err(409, 'conflict', 'node still serves clients');
        }
        extraNodes.remove(n);
        return _json({'ok': true});
      case ('GET', ['clients']):
        return _json([
          for (final c in clients) {...c, 'online': online.contains(c['id'])},
        ]);
      case ('PATCH', ['clients', final id]):
        final e = guard('admin');
        if (e != null) return e;
        final c = clients.where((x) => x['id'] == id).firstOrNull;
        if (c == null) return _err(404, 'not_found', 'not found');
        if (body.containsKey('name')) c['name'] = body['name'];
        if (body.containsKey('tags')) c['tags'] = body['tags'];
        if (body.containsKey('disabled')) {
          c['disabled_at'] = body['disabled'] == true ? 1700000300000 : null;
        }
        return _json({...c, 'online': online.contains(id)});
      case ('POST', ['clients', final id, 'restart']):
        final e = guard('admin');
        if (e != null) return e;
        if (!online.contains(id)) {
          return _err(409, 'conflict', 'the device is offline');
        }
        restarts.add(id);
        return _json({'ok': true});
      case ('POST', ['clients', final id, 'kick']):
        final e = guard('admin');
        if (e != null) return e;
        return clients.any((x) => x['id'] == id)
            ? _json({'ok': true})
            : _err(404, 'not_found', 'not found');
      case ('DELETE', ['clients', final id]):
        final e = guard('admin');
        if (e != null) return e;
        clients.removeWhere((x) => x['id'] == id);
        tunnels.removeWhere((x) => x['client_id'] == id);
        return _json({'ok': true});
      case ('GET', ['tunnels']):
        final cid = r.url.queryParameters['client_id'];
        return _json([
          for (final t in tunnels)
            if (cid == null || t['client_id'] == cid) t,
        ]);
      case ('POST', ['tunnels']):
        final e = guard('operator');
        if (e != null) return e;
        if (tunnels.any(
          (t) =>
              t['remote_port'] == body['remote_port'] &&
              body['remote_port'] != 0,
        )) {
          return _err(
            409,
            'conflict',
            'remote port ${body['remote_port']} is already used by another tunnel',
          );
        }
        final t = addTunnel(
          body['client_id']! as String,
          body['name']! as String,
          local: body['local_port']! as int,
          remote: (body['remote_port'] as int?) ?? 0,
          state: 'pending',
        );
        t['local_ip'] = body['local_ip'] ?? '127.0.0.1';
        t['kind'] = body['kind'] ?? 'tcp';
        t['custom_domains'] = body['custom_domains'] ?? <String>[];
        t['subdomain'] = body['subdomain'] ?? '';
        t['host_header_rewrite'] = body['host_header_rewrite'] ?? '';
        t['locations'] = body['locations'] ?? <String>[];
        t['http_user'] = body['http_user'] ?? '';
        t['route_by_http_user'] = body['route_by_http_user'] ?? '';
        t['request_headers'] = body['request_headers'] ?? <String, String>{};
        t['response_headers'] = body['response_headers'] ?? <String, String>{};
        t['proxy_protocol'] = body['proxy_protocol'] ?? 0;
        t['bandwidth_bps'] = body['bandwidth_bps'] ?? 0;
        t['bandwidth_server'] = body['bandwidth_server'] ?? false;
        t['lb_group'] = body['lb_group'] ?? '';
        t['lb_group_key_set'] =
            ((body['lb_group_key'] as String?) ?? '').isNotEmpty;
        if (body['health_check'] != null) {
          t['health_check'] = body['health_check'];
        }
        final pw = (body['http_password'] as String?) ?? '';
        t['http_password_set'] = pw.isNotEmpty;
        if (pw.isNotEmpty) httpPasswords[t['id']! as String] = pw;
        return _json(t, 201);
      case ('PATCH', ['tunnels', final id]):
        final e = guard('operator');
        if (e != null) return e;
        final t = tunnels.where((x) => x['id'] == id).firstOrNull;
        if (t == null) return _err(404, 'not_found', 'not found');
        for (final k in [
          'name',
          'local_ip',
          'local_port',
          'remote_port',
          'enabled',
          'custom_domains',
          'subdomain',
          'host_header_rewrite',
          'locations',
          'http_user',
          'route_by_http_user',
          'request_headers',
          'response_headers',
          'proxy_protocol',
          'bandwidth_bps',
          'bandwidth_server',
          'lb_group',
        ]) {
          if (body.containsKey(k)) t[k] = body[k];
        }
        if (body.containsKey('lb_group_key')) {
          t['lb_group_key_set'] =
              ((body['lb_group_key'] as String?) ?? '').isNotEmpty;
        }
        if (body.containsKey('health_check')) {
          final h = body['health_check'] as Map<String, dynamic>?;
          if (h == null || h['kind'] == '') {
            t.remove('health_check');
          } else {
            t['health_check'] = h;
          }
        }
        if (body.containsKey('http_password')) {
          final pw = body['http_password']! as String;
          t['http_password_set'] = pw.isNotEmpty;
          if (pw.isEmpty) {
            httpPasswords.remove(id);
          } else {
            httpPasswords[id] = pw;
          }
        }
        return _json(t);
      case ('GET', ['visitors']):
        final cid = r.url.queryParameters['client_id'];
        return _json([
          for (final v in visitors)
            if (cid == null || v['client_id'] == cid) v,
        ]);
      case ('POST', ['visitors']):
        final e = guard('operator');
        if (e != null) return e;
        if (visitors.any(
          (v) =>
              v['client_id'] == body['client_id'] && v['name'] == body['name'],
        )) {
          return _err(409, 'conflict', 'visitor name already used');
        }
        final v = addVisitor(
          body['client_id']! as String,
          body['name']! as String,
          body['tunnel_id']! as String,
          addr: (body['bind_addr'] as String?) ?? '127.0.0.1',
          port: (body['bind_port'] as int?) ?? 0,
          enabled: (body['enabled'] as bool?) ?? true,
        );
        return _json(v, 201);
      case ('PATCH', ['visitors', final id]):
        final e = guard('operator');
        if (e != null) return e;
        final v = visitors.where((x) => x['id'] == id).firstOrNull;
        if (v == null) return _err(404, 'not_found', 'not found');
        for (final k in [
          'name',
          'tunnel_id',
          'bind_addr',
          'bind_port',
          'enabled',
        ]) {
          if (body.containsKey(k)) v[k] = body[k];
        }
        return _json(v);
      case ('DELETE', ['visitors', final id]):
        final e = guard('operator');
        if (e != null) return e;
        visitors.removeWhere((x) => x['id'] == id);
        return _json({'ok': true});
      case ('DELETE', ['tunnels', final id]):
        final e = guard('operator');
        if (e != null) return e;
        tunnels.removeWhere((x) => x['id'] == id);
        return _json({'ok': true});
      case ('GET', ['enroll-tokens']):
        final e = guard('admin');
        return e ?? _json(tokens);
      case ('POST', ['enroll-tokens']):
        final e = guard('admin');
        if (e != null) return e;
        final id = _id();
        tokens.add({
          'id': id,
          'name': body['name'],
          'created_by': adminName,
          'created_at': 1700000000000,
          'expires_at': DateTime.now().millisecondsSinceEpoch + 3600000,
          'used_at': null,
          'client_id': null,
          'revoked_at': null,
          'kind': body['kind'] ?? 'client',
          'node_id': body['node_id'] ?? '',
        });
        const token = 'rfrp_secret_token';
        if (body['kind'] == 'node') {
          return _json({
            'id': id,
            'token': token,
            'kind': 'node',
            'expires_at': DateTime.now().millisecondsSinceEpoch + 3600000,
            'commands': {
              'controller': '$publicHost:7001',
              'direct':
                  'rustfrp-agent --mode edge --controller $publicHost:7001 --enroll-token $token --public-host <this-node-public-host>',
            },
          }, 201);
        }
        return _json({
          'id': id,
          'token': token,
          'expires_at': DateTime.now().millisecondsSinceEpoch + 3600000,
          'commands': {
            'controller': '$publicHost:7000',
            'direct':
                'rustfrp-agent --mode client --controller $publicHost:7000 --enroll-token $token --name "${body['name']}"',
            if (publicUrl.isNotEmpty)
              'linux_macos':
                  'curl -fsSL $publicUrl/install.sh | sh -s -- --enroll-token $token',
            if (publicUrl.isNotEmpty)
              'windows': 'powershell -c "iwr $publicUrl/install.ps1 | iex"',
          },
        }, 201);
      case ('DELETE', ['enroll-tokens', final id]):
        final e = guard('admin');
        if (e != null) return e;
        final t = tokens.where((x) => x['id'] == id).firstOrNull;
        if (t == null) return _err(404, 'not_found', 'not found');
        t['revoked_at'] = 1700000400000;
        return _json({'ok': true});
      case ('GET', ['audit']):
        final e = guard('admin');
        return e ?? _json(audit);
      case ('GET', ['admins']):
        final e = guard('owner');
        return e ?? _json([_admin, ...extraAdmins]);
      case ('POST', ['admins']):
        final e = guard('owner');
        if (e != null) return e;
        if (extraAdmins.any((a) => a['name'] == body['name'])) {
          return _err(409, 'conflict', 'admin already exists');
        }
        final a = {
          'id': _id(),
          'name': body['name'],
          'role': body['role'],
          'disabled_at': null,
          'created_at': 3,
        };
        extraAdmins.add(a);
        return _json(a, 201);
      case ('PATCH', ['admins', final id]):
        final e = guard('owner');
        if (e != null) return e;
        final a = extraAdmins.where((x) => x['id'] == id).firstOrNull;
        if (a == null) return _err(404, 'not_found', 'not found');
        if (body.containsKey('role')) a['role'] = body['role'];
        if (body.containsKey('disabled')) {
          a['disabled_at'] = body['disabled'] == true ? 9 : null;
        }
        return _json(a);
    }
    return _err(404, 'not_found', 'not found');
  }
}
