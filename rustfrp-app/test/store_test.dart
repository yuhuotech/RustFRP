import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/core/models.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'support/fake_server.dart';

Future<ProviderContainer> container(
  FakeServer s, {
  bool sameOrigin = true,
  Map<String, Object> prefs = const {},
}) async {
  SharedPreferences.setMockInitialValues(prefs);
  final p = await SharedPreferences.getInstance();
  final c = ProviderContainer(
    retry: (_, _) => null,
    overrides: [
      prefsProvider.overrideWithValue(p),
      apiFactoryProvider.overrideWithValue((base) => s.api(base)),
      sameOriginProvider.overrideWithValue(sameOrigin),
      tickProvider.overrideWith((ref) => const Stream<int>.empty()),
    ],
  );
  addTearDown(c.dispose);
  return c;
}

void main() {
  test('desktop restore with no valid server ends at login', () async {
    for (final url in ['', 'not a url', 'http:///missing-host']) {
      final c = await container(
        FakeServer(),
        sameOrigin: false,
        prefs: {'server_url': url},
      );
      await c.read(sessionProvider.notifier).restore();
      expect(c.read(sessionProvider).status, SessionStatus.anonymous);
    }
    final c = await container(
      FakeServer()..loggedIn = true,
      sameOrigin: false,
      prefs: {'server_url': 'https://example.test'},
    );
    await c.read(sessionProvider.notifier).restore();
    expect(c.read(sessionProvider).status, SessionStatus.authenticated);
  });

  test(
    'session: restore without a cookie → anonymous, with one → authenticated',
    () async {
      final s = FakeServer();
      final c = await container(s);
      expect(c.read(sessionProvider).status, SessionStatus.unknown);
      await c.read(sessionProvider.notifier).restore();
      expect(c.read(sessionProvider).status, SessionStatus.anonymous);

      s.loggedIn = true;
      await c.read(sessionProvider.notifier).restore();
      expect(c.read(sessionProvider).status, SessionStatus.authenticated);
      expect(c.read(sessionProvider).role, Role.owner);

      s.offline = true;
      await c.read(sessionProvider.notifier).restore();
      expect(
        c.read(sessionProvider).status,
        SessionStatus.anonymous,
        reason: 'an unreachable server never leaves the app half logged in',
      );
      expect(c.read(sessionProvider).admin, isNull);
    },
  );

  test('login (same origin): success, wrong password, logout', () async {
    final s = FakeServer();
    final c = await container(s);
    final n = c.read(sessionProvider.notifier);
    expect(await n.login('root', 'bad'), 'invalid user name or password');
    expect(c.read(sessionProvider).status, isNot(SessionStatus.authenticated));
    expect(await n.login(' root ', s.adminPassword), isNull);
    expect(c.read(sessionProvider).status, SessionStatus.authenticated);
    expect(c.read(sessionProvider).admin?.name, 'root');
    await n.logout();
    expect(c.read(sessionProvider).status, SessionStatus.anonymous);
    // Logging out when the network is down still ends the local session.
    await n.login('root', s.adminPassword);
    s.offline = true;
    await n.logout();
    expect(c.read(sessionProvider).status, SessionStatus.anonymous);
  });

  test(
    'login (desktop): validates and remembers the controller address',
    () async {
      final s = FakeServer();
      final c = await container(s, sameOrigin: false);
      final n = c.read(sessionProvider.notifier);
      for (final bad in ['', 'frp.example.com', 'ftp://x', 'http://']) {
        expect(
          await n.login('root', s.adminPassword, serverUrl: bad),
          contains('服务器地址'),
          reason: bad,
        );
      }
      expect(
        s.requests,
        isEmpty,
        reason: 'nothing is sent for an invalid address',
      );
      expect(
        await n.login(
          'root',
          s.adminPassword,
          serverUrl: 'https://frp.example.com//',
        ),
        isNull,
      );
      expect(c.read(sessionProvider).serverUrl, 'https://frp.example.com');
      expect(s.requests.first.url.origin, 'https://frp.example.com');
      expect(
        (await SharedPreferences.getInstance()).getString('server_url'),
        'https://frp.example.com',
      );

      // A new container (app restart) picks the remembered address up.
      final again = await container(
        s,
        sameOrigin: false,
        prefs: {'server_url': 'https://saved.example.com'},
      );
      expect(
        again.read(sessionProvider).serverUrl,
        'https://saved.example.com',
      );
      expect(again.read(apiProvider).baseUrl, 'https://saved.example.com');
      expect(
        await again
            .read(sessionProvider.notifier)
            .login('root', s.adminPassword),
        isNull,
        reason: 'falls back to the saved address',
      );
    },
  );

  test(
    'an expired session logs the user out when any call returns 401',
    () async {
      final s = FakeServer();
      final c = await container(s);
      await c.read(sessionProvider.notifier).login('root', s.adminPassword);
      s.loggedIn = false;
      await expectLater(
        c.read(apiProvider).clients(),
        throwsA(isA<ApiException>()),
      );
      expect(c.read(sessionProvider).status, SessionStatus.anonymous);
      // Expiring while already anonymous is a no-op.
      c.read(sessionProvider.notifier).expire();
      expect(c.read(sessionProvider).status, SessionStatus.anonymous);
    },
  );

  test('navigation and selection state', () async {
    final c = await container(FakeServer());
    expect(c.read(sectionProvider), Section.overview);
    c.read(sectionProvider.notifier).select(Section.tunnels);
    expect(c.read(sectionProvider), Section.tunnels);
    c.read(selectedClientProvider.notifier).select('c1');
    expect(c.read(selectedClientProvider), 'c1');
    c.read(selectedSettingProvider.notifier).select('about');
    expect(c.read(selectedSettingProvider), 'about');
    expect(c.read(tunnelFilterProvider), isNull);
    c.read(tunnelFilterProvider.notifier).select(TunnelState.error);
    expect(c.read(tunnelFilterProvider), TunnelState.error);
  });

  test('theme mode persists', () async {
    final c = await container(FakeServer());
    expect(c.read(themeModeProvider), ThemeMode.system);
    await c.read(themeModeProvider.notifier).set(ThemeMode.dark);
    expect(c.read(themeModeProvider), ThemeMode.dark);
    expect(
      (await SharedPreferences.getInstance()).getString('theme_mode'),
      'dark',
    );
    for (final (stored, mode) in [
      ('light', ThemeMode.light),
      ('dark', ThemeMode.dark),
      ('x', ThemeMode.system),
    ]) {
      expect(
        (await container(
          FakeServer(),
          prefs: {'theme_mode': stored},
        )).read(themeModeProvider),
        mode,
      );
    }
  });

  test('data providers load, and actions refresh them', () async {
    final s = FakeServer();
    final cl = s.addClient('nas', isOnline: true);
    final c = await container(s);
    await c.read(sessionProvider.notifier).login('root', s.adminPassword);
    c.listen(clientsProvider, (_, _) {});
    c.listen(tunnelsProvider, (_, _) {});
    c.listen(overviewProvider, (_, _) {});
    expect((await c.read(clientsProvider.future)).single.name, 'nas');
    expect((await c.read(overviewProvider.future)).clients, 1);
    expect(await c.read(tunnelsProvider.future), isEmpty);

    final a = c.read(actionsProvider);
    final id = cl['id']! as String;
    final t = await a.createTunnel(
      clientId: id,
      name: 'ssh',
      localPort: 22,
      remotePort: 6000,
      reason: 'r',
    );
    expect((await c.read(tunnelsProvider.future)).single.name, 'ssh');
    await a.updateTunnel(t.id, enabled: false, reason: 'r');
    expect((await c.read(tunnelsProvider.future)).single.enabled, isFalse);
    await a.deleteTunnel(t.id, 'r');
    expect(await c.read(tunnelsProvider.future), isEmpty);

    expect((await a.renameClient(id, 'renamed', 'r')).name, 'renamed');
    expect((await a.setClientDisabled(id, true, 'r')).disabled, isTrue);
    await a.kickClient(id, 'r');
    final r = await a.createToken('box', 60, 'r');
    expect(r.token, startsWith('rfrp_'));
    c.listen(tokensProvider, (_, _) {});
    expect((await c.read(tokensProvider.future)).length, 1);
    await a.revokeToken(r.id, 'r');
    await a.deleteClient(id, 'r');
    expect(await c.read(clientsProvider.future), isEmpty);
    c.listen(auditProvider, (_, _) {});
    c.listen(adminsProvider, (_, _) {});
    c.listen(nodeProvider, (_, _) {});
    expect(await c.read(auditProvider.future), isEmpty);
    expect((await c.read(adminsProvider.future)).length, 2);
    expect((await c.read(nodeProvider.future)).edgePort, 7000);
  });

  test(
    'a failing write surfaces the server message and refreshes nothing',
    () async {
      final s = FakeServer(role: 'viewer');
      final c = await container(s);
      await c.read(sessionProvider.notifier).login('root', s.adminPassword);
      final a = c.read(actionsProvider);
      await expectLater(
        a.deleteTunnel('x', 'r'),
        throwsA(
          isA<ApiException>().having(
            (e) => e.message,
            'message',
            'insufficient permissions',
          ),
        ),
      );
    },
  );

  test('SessionState.copyWith keeps and clears the admin', () {
    const a = Admin(id: '1', name: 'n', role: Role.admin);
    const s = SessionState(
      status: SessionStatus.authenticated,
      admin: a,
      serverUrl: 'u',
    );
    expect(s.copyWith(serverUrl: 'v').admin, a);
    expect(s.copyWith(clearAdmin: true).admin, isNull);
    expect(const SessionState().role, Role.viewer);
  });
}
