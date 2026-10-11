import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/models.dart';

void main() {
  test('Role parsing is forward compatible and ordered', () {
    expect(Role.fromWire('owner'), Role.owner);
    expect(Role.fromWire('admin'), Role.admin);
    expect(Role.fromWire('operator'), Role.operator);
    expect(Role.fromWire('viewer'), Role.viewer);
    expect(
      Role.fromWire('root'),
      Role.viewer,
      reason: 'unknown roles get the lowest privilege',
    );
    expect(Role.fromWire(null), Role.viewer);
    expect(Role.admin.atLeast(Role.operator), isTrue);
    expect(Role.operator.atLeast(Role.admin), isFalse);
    expect(Role.values.map((r) => r.label).toSet().length, 4);
  });

  test('Admin', () {
    final a = Admin.fromJson({
      'id': 'a',
      'name': 'root',
      'role': 'owner',
      'disabled_at': 5,
      'created_at': 9,
    });
    expect(
      (a.name, a.role, a.disabledAt, a.createdAt),
      ('root', Role.owner, 5, 9),
    );
    expect(
      a,
      Admin.fromJson({
        'id': 'a',
        'name': 'root',
        'role': 'owner',
        'disabled_at': 5,
        'created_at': 9,
      }),
    );
    expect(a.hashCode, isNot(0));
    expect(Admin.fromJson({'id': 'x'}).role, Role.viewer);
    expect(
      () => Admin.fromJson({'name': 'no id'}),
      throwsA(isA<ProtocolException>()),
    );
    expect(() => Admin.fromJson('nope'), throwsA(isA<ProtocolException>()));
    expect(ProtocolException('x').toString(), contains('x'));
  });

  test('ClientDevice tolerates missing fields and compares by value', () {
    final c = ClientDevice.fromJson({
      'id': 'c1',
      'name': 'nas',
      'os': 'linux',
      'arch': 'arm64',
      'version': '0.1',
      'revision': 3,
      'created_at': 10,
      'last_seen_at': 20,
      'disabled_at': null,
      'online': true,
    });
    expect(
      (c.name, c.revision, c.lastSeenAt, c.online, c.disabled),
      ('nas', 3, 20, true, false),
    );
    final minimal = ClientDevice.fromJson({'id': 'c2'});
    expect(
      (minimal.name, minimal.os, minimal.online, minimal.lastSeenAt),
      ('', '', false, null),
    );
    expect(
      ClientDevice.fromJson({'id': 'c2', 'disabled_at': 1}).disabled,
      isTrue,
    );
    expect(
      c ==
          ClientDevice.fromJson({
            'id': 'c1',
            'name': 'nas',
            'os': 'linux',
            'arch': 'arm64',
            'version': '0.1',
            'revision': 3,
            'created_at': 10,
            'last_seen_at': 20,
            'online': true,
          }),
      isTrue,
    );
    expect(c == minimal, isFalse);
    expect(c.hashCode, isNot(minimal.hashCode));
    expect(
      () => ClientDevice.fromJson(<String, Object?>{}),
      throwsA(isA<ProtocolException>()),
    );
  });

  test('Tunnel', () {
    final t = Tunnel.fromJson({
      'id': 't1',
      'client_id': 'c1',
      'name': 'ssh',
      'kind': 'tcp',
      'local_ip': '10.0.0.1',
      'local_port': 22,
      'remote_port': 6000,
      'enabled': false,
      'created_at': 1,
      'updated_at': 2,
      'state': 'error',
      'status_port': 0,
      'status_error': 'boom',
    });
    expect(
      (t.localIp, t.localPort, t.enabled, t.state, t.statusError),
      ('10.0.0.1', 22, false, TunnelState.error, 'boom'),
    );
    expect(
      t.publicPort,
      6000,
      reason: 'configured port is shown until the live one is known',
    );
    expect(
      Tunnel.fromJson({'id': 't', 'status_port': 6123, 'remote_port': 0})
          .publicPort,
      6123,
    );
    final tagged = ClientDevice.fromJson({
      'id': 'c',
      'tags': ['a', 3, 'b'],
    });
    expect(tagged.tags, ['a', 'b']);
    expect(
      tagged ==
          ClientDevice.fromJson({
            'id': 'c',
            'tags': ['a', 'b'],
          }),
      isTrue,
    );
    expect(tagged == ClientDevice.fromJson({'id': 'c'}), isFalse);
    expect(tagged.hashCode, isA<int>());
    final h = Tunnel.fromJson({
      'id': 'h',
      'locations': ['/api', 3, '/rpc'],
      'http_user': 'alice',
      'http_password_set': true,
      'route_by_http_user': 'bob',
      'request_headers': {'X-A': '1', 'bad': 2, 3: 'x'},
    });
    expect(h.locations, ['/api', '/rpc']);
    expect(
      (h.httpUser, h.httpPasswordSet, h.routeByHttpUser),
      ('alice', true, 'bob'),
    );
    expect(h.requestHeaders, {'X-A': '1'});
    expect(
      ClientDevice.fromJson({'id': 'c', 'last_ip': '198.51.100.2'}).lastIp,
      '198.51.100.2',
    );
    expect(ClientDevice.fromJson({'id': 'c'}).lastIp, '');
    expect(
      Tunnel.fromJson({
        'id': 'r',
        'response_headers': {'X-B': '2', 'bad': 3},
      }).responseHeaders,
      {'X-B': '2'},
    );
    expect(Tunnel.fromJson({'id': 'p', 'proxy_protocol': 2}).proxyProtocol, 2);
    expect(h == Tunnel.fromJson({'id': 'h', 'proxy_protocol': 1}), isFalse);
    final lb = Tunnel.fromJson({
      'id': 'l',
      'lb_group': 'g',
      'lb_group_key_set': true,
      'health_check': {
        'kind': 'http',
        'timeout_secs': 2,
        'max_failed': 3,
        'interval_secs': 5,
        'path': '/h',
        'headers': {'X-A': '1', 'bad': 2},
      },
    });
    expect((lb.lbGroup, lb.lbGroupKeySet), ('g', true));
    expect(lb.healthCheck?.headers, {'X-A': '1'});
    expect(lb.healthCheck?.toJson()['interval_secs'], 5);
    expect(
      lb ==
          Tunnel.fromJson({
            'id': 'l',
            'lb_group': 'g',
            'lb_group_key_set': true,
          }),
      isFalse,
    );
    expect(lb.healthCheck, isNot(const HealthCheck(kind: 'tcp')));
    expect(lb.healthCheck.hashCode, lb.healthCheck.hashCode);
    final bw = Tunnel.fromJson({
      'id': 'b',
      'bandwidth_bps': 65536,
      'bandwidth_server': true,
    });
    expect((bw.bandwidthBps, bw.bandwidthServer), (65536, true));
    expect(bw == Tunnel.fromJson({'id': 'b', 'bandwidth_bps': 65536}), isFalse);
    expect(h == Tunnel.fromJson({'id': 'h'}), isFalse);
    expect(
      h,
      Tunnel.fromJson({
        'id': 'h',
        'locations': ['/api', '/rpc'],
        'http_user': 'alice',
        'http_password_set': true,
        'route_by_http_user': 'bob',
        'request_headers': {'X-A': '1'},
      }),
    );
    expect(h.hashCode, isA<int>());
    final d = Tunnel.fromJson({'id': 't2'});
    expect(d.locations, isEmpty);
    expect(d.httpPasswordSet, isFalse);
    expect(d.requestHeaders, isEmpty);
    expect(
      (d.kind, d.localIp, d.enabled, d.state),
      ('tcp', '127.0.0.1', true, TunnelState.unknown),
      reason: 'a missing state is treated as unknown',
    );
    expect(
      t ==
          Tunnel.fromJson({
            'id': 't1',
            'client_id': 'c1',
            'name': 'ssh',
            'kind': 'tcp',
            'local_ip': '10.0.0.1',
            'local_port': 22,
            'remote_port': 6000,
            'enabled': false,
            'created_at': 1,
            'updated_at': 2,
            'state': 'error',
            'status_error': 'boom',
          }),
      isTrue,
    );
    expect(t == d, isFalse);
    expect(t.hashCode == d.hashCode, isFalse);
    final w = Tunnel.fromJson({
      'id': 'w',
      'kind': 'http',
      'custom_domains': ['a.com', 5],
      'subdomain': 's',
      'host_header_rewrite': 'h',
    });
    expect((w.isWeb, w.subdomain, w.hostHeaderRewrite), (true, 's', 'h'));
    expect(w.customDomains, ['a.com']);
    expect(
      w ==
          Tunnel.fromJson({
            'id': 'w',
            'kind': 'http',
            'custom_domains': ['a.com'],
            'subdomain': 's',
            'host_header_rewrite': 'h',
          }),
      isTrue,
    );
    expect(
      w ==
          Tunnel.fromJson({
            'id': 'w',
            'kind': 'http',
            'custom_domains': ['b.com'],
            'subdomain': 's',
            'host_header_rewrite': 'h',
          }),
      isFalse,
    );
    expect(t.isWeb, isFalse);
    expect(() => Tunnel.fromJson({}), throwsA(isA<ProtocolException>()));
  });

  test('TunnelState', () {
    for (final s in ['pending', 'running', 'error', 'stopped']) {
      expect(TunnelState.fromWire(s).name, s);
    }
    expect(TunnelState.fromWire('whatever'), TunnelState.unknown);
    expect(TunnelState.fromWire(null), TunnelState.unknown);
    expect(
      TunnelState.values.map((s) => s.label).toSet().length,
      TunnelState.values.length,
    );
  });

  test('EnrollToken status is derived at a given time', () {
    EnrollToken tok({int? used, int? revoked, int expires = 1000}) =>
        EnrollToken.fromJson({
          'id': 'k',
          'name': 'n',
          'expires_at': expires,
          'used_at': used,
          'revoked_at': revoked,
          'client_id': used == null ? null : 'c',
        });
    expect(tok().status(500), 'pending');
    expect(tok().status(1000), 'expired');
    expect(tok(used: 1).status(500), 'used');
    expect(tok(revoked: 1).status(500), 'revoked');
    expect(tok(used: 1, revoked: 2).status(5000), 'used');
    expect(tok(used: 1).clientId, 'c');
    expect(() => EnrollToken.fromJson({}), throwsA(isA<ProtocolException>()));
  });

  test('EnrollResult keeps only string commands', () {
    final r = EnrollResult.fromJson({
      'id': 'i',
      'token': 'rfrp_x',
      'expires_at': 9,
      'commands': {'direct': 'cmd', 'weird': 5},
    });
    expect((r.token, r.expiresAt), ('rfrp_x', 9));
    expect(r.commands, {'direct': 'cmd'});
    expect(
      () => EnrollResult.fromJson({'id': 'i'}),
      throwsA(isA<ProtocolException>()),
    );
  });

  test('AuditEntry, Overview, NodeInfo', () {
    final a = AuditEntry.fromJson({
      'id': 3,
      'ts': 5,
      'actor': 'root',
      'action': 'tunnel.create',
      'target_kind': 'tunnel',
      'target_id': 't',
      'reason': 'r',
      'result': 'ok',
      'ip': '1.1.1.1',
    });
    expect((a.id, a.actor, a.ip), (3, 'root', '1.1.1.1'));
    expect(AuditEntry.fromJson(null).action, '');

    final o = Overview.fromJson({
      'counts': {
        'clients': 2,
        'tunnels': 3,
        'tunnels_running': 1,
        'tokens_pending': 4,
      },
      'clients_online': 1,
      'version': '0.1',
    });
    expect(
      (
        o.clients,
        o.tunnels,
        o.tunnelsRunning,
        o.tokensPending,
        o.clientsOnline,
        o.version,
      ),
      (2, 3, 1, 4, 1, '0.1'),
    );
    expect(Overview.fromJson({}).clients, 0);

    final n = NodeInfo.fromJson({
      'name': 'builtin',
      'public_host': 'h',
      'edge_port': 7100,
      'allow_ports': [
        [6000, 6100],
        [7000, 7000],
        [1],
      ],
      'version': 'v',
      'clients_online': 2,
    });
    expect(n.allowPorts, [(6000, 6100), (7000, 7000)]);
    expect(n.allowPortsLabel, '6000-6100, 7000');
    expect((n.edgePort, n.clientsOnline), (7100, 2));
    expect(NodeInfo.fromJson({}).allowPortsLabel, '不限制');
    expect(NodeInfo.fromJson({}).edgePort, 7000);
    expect(NodeInfo.fromJson({'enabled': false}).enabled, isFalse);
    expect(NodeInfo.fromJson({}).enabled, isTrue);
    final v = NodeInfo.fromJson({
      'vhost_http_port': 8080,
      'vhost_https_port': null,
      'subdomain_host': 't.com',
    });
    expect(
      (v.vhostHttpPort, v.vhostHttpsPort, v.subdomainHost),
      (8080, null, 't.com'),
    );
  });

  test('ApiException', () {
    final e = ApiException(401, 'unauthorized', '请登录');
    expect((e.unauthorized, e.toString()), (true, '请登录'));
    expect(ApiException(500, 'x', 'y').unauthorized, isFalse);
  });
}
