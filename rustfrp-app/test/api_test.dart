import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:rustfrp_app/core/api.dart';
import 'package:rustfrp_app/core/models.dart';

import 'support/fake_server.dart';

void main() {
  late FakeServer server;
  late ApiClient api;
  setUp(() {
    server = FakeServer();
    api = server.api('https://frp.example.com');
  });

  Future<void> login() => api.login(server.adminName, server.adminPassword);

  test('login stores the CSRF token, replays the cookie and sends the panel header', () async {
    final admin = await api.login(server.adminName, server.adminPassword);
    expect((admin.name, admin.role), ('root', Role.owner));
    expect(api.hasSession, isTrue);
    expect(server.requests.single.headers['x-rustfrp-admin'], '1');
    expect(
      server.requests.single.url.toString(),
      'https://frp.example.com/api/v1/auth/login',
    );

    await api.overview();
    expect(server.requests.last.headers['cookie'], 'rfrp_session=tok123');
    expect(
      server.requests.last.headers['x-csrf-token'],
      isNull,
      reason: 'reads do not need CSRF',
    );

    await api.kickClient(server.addClient('c')['id']! as String, reason: 'r');
    expect(server.requests.last.headers['x-csrf-token'], 'csrf-token');
    expect(
      server.requests.last.headers['content-type'],
      contains('application/json'),
    );
  });

  test(
    'wrong credentials raise ApiException without clearing anything',
    () async {
      var unauthorizedCalls = 0;
      api.onUnauthorized = () => unauthorizedCalls++;
      await expectLater(
        api.login('root', 'nope'),
        throwsA(
          isA<ApiException>().having(
            (e) => e.code,
            'code',
            'invalid_credentials',
          ),
        ),
      );
      expect(
        unauthorizedCalls,
        0,
        reason: 'a failed login is not an expired session',
      );
      expect(api.hasSession, isFalse);
    },
  );

  test(
    'me() restores a session or reports none; errors other than 401 propagate',
    () async {
      expect(await api.me(), isNull);
      server.loggedIn = true;
      final a = await api.me();
      expect(a?.name, 'root');
      expect(api.csrf, 'csrf-token');
      server.failNext = true;
      await expectLater(
        api.me(),
        throwsA(isA<ApiException>().having((e) => e.status, 'status', 500)),
      );
    },
  );

  test(
    'a 401 on an authenticated call clears the session and notifies',
    () async {
      await login();
      var called = 0;
      api.onUnauthorized = () => called++;
      server.loggedIn = false;
      await expectLater(
        api.clients(),
        throwsA(
          isA<ApiException>().having(
            (e) => e.unauthorized,
            'unauthorized',
            true,
          ),
        ),
      );
      expect(called, 1);
      expect(api.hasSession, isFalse);
    },
  );

  test('logout clears local state even if the server call fails', () async {
    await login();
    await api.logout();
    expect(api.hasSession, isFalse);
    await login();
    server.offline = true;
    await expectLater(api.logout(), throwsA(isA<ApiException>()));
    expect(api.hasSession, isFalse);
    // A cleared cookie really is dropped: the next request goes out without one.
    server.offline = false;
    server.loggedIn = true;
    await api.overview();
    expect(server.requests.last.headers.containsKey('cookie'), isFalse);
  });

  test(
    'network failures, timeouts and non-JSON bodies become ApiExceptions',
    () async {
      server.offline = true;
      await expectLater(
        api.overview(),
        throwsA(isA<ApiException>().having((e) => e.code, 'code', 'network')),
      );
      server.offline = false;

      final slow = ApiClient(
        baseUrl: '',
        timeout: const Duration(milliseconds: 20),
        client: MockClient((_) => Completer<http.Response>().future),
      );
      await expectLater(
        slow.overview(),
        throwsA(isA<ApiException>().having((e) => e.code, 'code', 'timeout')),
      );

      final html = ApiClient(
        baseUrl: '',
        client: MockClient(
          (_) async => http.Response('<html>bad gateway</html>', 502),
        ),
      );
      await expectLater(
        html.overview(),
        throwsA(isA<ApiException>().having((e) => e.code, 'code', 'http_502')),
      );
      final garbage = ApiClient(
        baseUrl: '',
        client: MockClient((_) async => http.Response('not json', 200)),
      );
      expect(
        await garbage.overview(),
        isA<Overview>(),
        reason: 'an unparseable success body degrades to defaults',
      );
      final notList = ApiClient(
        baseUrl: '',
        client: MockClient((_) async => http.Response('{"x":1}', 200)),
      );
      await expectLater(notList.clients(), throwsA(isA<ProtocolException>()));
    },
  );

  test('reads', () async {
    await login();
    final c = server.addClient('nas', isOnline: true);
    server.addTunnel(c['id']! as String, 'ssh');
    server.audit.add({
      'id': 1,
      'ts': 1,
      'actor': 'root',
      'action': 'a',
      'result': 'ok',
    });
    expect((await api.overview()).clientsOnline, 1);
    expect((await api.node()).publicHost, 'frp.example.com');
    expect((await api.clients()).single.online, isTrue);
    expect((await api.tunnels()).single.name, 'ssh');
    expect(await api.tunnels(clientId: 'other'), isEmpty);
    expect(server.requests.last.url.queryParameters['client_id'], 'other');
    expect((await api.audit()).single.action, 'a');
    await api.audit(limit: 5, before: 9);
    expect(server.requests.last.url.queryParameters, {
      'limit': '5',
      'before': '9',
    });
    expect((await api.admins()).length, 2);
    expect(await api.enrollTokens(), isEmpty);
  });

  test('writes send reasons and only the fields that changed', () async {
    await login();
    final c = server.addClient('nas');
    final id = c['id']! as String;

    final renamed = await api.patchClient(id, name: 'new', reason: 'tidy');
    expect(renamed.name, 'new');
    expect(jsonDecode(server.requests.last.body), {
      'name': 'new',
      'reason': 'tidy',
    });
    expect(
      (await api.patchClient(id, disabled: true, reason: 'x')).disabled,
      isTrue,
    );
    expect(jsonDecode(server.requests.last.body), {
      'disabled': true,
      'reason': 'x',
    });

    final t = await api.createTunnel(
      clientId: id,
      name: 'ssh',
      localPort: 22,
      remotePort: 6000,
      reason: 'r',
    );
    expect((t.name, t.remotePort), ('ssh', 6000));
    final upd = await api.patchTunnel(
      t.id,
      name: 'ssh2',
      localIp: '10.0.0.2',
      localPort: 2222,
      remotePort: 6001,
      enabled: false,
      reason: 'r',
    );
    expect(
      (upd.name, upd.localIp, upd.localPort, upd.enabled),
      ('ssh2', '10.0.0.2', 2222, false),
    );
    await api.patchTunnel(t.id, enabled: true, reason: 'r');
    expect(jsonDecode(server.requests.last.body), {
      'enabled': true,
      'reason': 'r',
    });

    await api.deleteTunnel(t.id, reason: 'bye');
    expect(server.requests.last.url.queryParameters, {'reason': 'bye'});
    await expectLater(
      api.deleteTunnel(t.id, reason: ''),
      throwsA(isA<ApiException>().having((e) => e.status, 'status', 400)),
    );

    final tok = await api.createEnrollToken(
      name: 'box',
      ttlMinutes: 30,
      reason: 'r',
    );
    expect(tok.token, startsWith('rfrp_'));
    expect(tok.commands['direct'], contains('--enroll-token'));
    await api.revokeEnrollToken(tok.id, reason: 'oops');
    await api.deleteClient(id, reason: 'gone');
    expect(await api.clients(), isEmpty);
  });

  test(
    'server-side permission and validation errors keep the server message',
    () async {
      server.role = 'viewer';
      await login();
      await expectLater(
        api.createTunnel(clientId: 'x', name: 'n', localPort: 1, reason: 'r'),
        throwsA(
          isA<ApiException>().having(
            (e) => e.message,
            'message',
            'insufficient permissions',
          ),
        ),
      );
      server.role = 'owner';
      final c = server.addClient('nas');
      await api.createTunnel(
        clientId: c['id']! as String,
        name: 'a',
        localPort: 22,
        remotePort: 6000,
        reason: 'r',
      );
      await expectLater(
        api.createTunnel(
          clientId: c['id']! as String,
          name: 'b',
          localPort: 23,
          remotePort: 6000,
          reason: 'r',
        ),
        throwsA(isA<ApiException>().having((e) => e.status, 'status', 409)),
      );
    },
  );

  test('close releases the underlying client', () {
    api.close();
  });
}
