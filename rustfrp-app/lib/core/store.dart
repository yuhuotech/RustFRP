import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'api.dart';
import 'models.dart';

// ───────────── infrastructure ─────────────

/// Overridden in `main()` (and in tests).
final prefsProvider = Provider<SharedPreferences>(
  (ref) => throw UnimplementedError('prefsProvider must be overridden'),
);

/// How API clients are built; tests swap in a client backed by a fake server.
final apiFactoryProvider = Provider<ApiClient Function(String baseUrl)>(
  (ref) =>
      (u) => ApiClient(baseUrl: u),
);

/// Whether the app runs in a browser served by the controller (same origin, no server URL to enter).
final sameOriginProvider = Provider<bool>((ref) => false);

/// Periodic refresh tick for live data (the controller has no push channel for the panel yet).
final tickProvider = StreamProvider<int>(
  (ref) => Stream.periodic(const Duration(seconds: 5), (i) => i),
);

const _kServer = 'server_url';
const _kTheme = 'theme_mode';

// ───────────── session ─────────────

enum SessionStatus { unknown, anonymous, authenticated }

@immutable
class SessionState {
  const SessionState({
    this.status = SessionStatus.unknown,
    this.admin,
    this.serverUrl = '',
  });
  final SessionStatus status;
  final Admin? admin;
  final String serverUrl;

  SessionState copyWith({
    SessionStatus? status,
    Admin? admin,
    String? serverUrl,
    bool clearAdmin = false,
  }) => SessionState(
    status: status ?? this.status,
    admin: clearAdmin ? null : (admin ?? this.admin),
    serverUrl: serverUrl ?? this.serverUrl,
  );

  Role get role => admin?.role ?? Role.viewer;
}

class ApiNotifier extends Notifier<ApiClient> {
  @override
  ApiClient build() {
    final base = ref.watch(sameOriginProvider)
        ? ''
        : (ref.read(prefsProvider).getString(_kServer) ?? '');
    final c = ref.read(apiFactoryProvider)(base);
    c.onUnauthorized = () => ref.read(sessionProvider.notifier).expire();
    return c;
  }

  /// Points the client at another controller (desktop login screen).
  void useServer(String baseUrl) {
    final c = ref.read(apiFactoryProvider)(baseUrl);
    c.onUnauthorized = () => ref.read(sessionProvider.notifier).expire();
    state = c;
  }
}

final apiProvider = NotifierProvider<ApiNotifier, ApiClient>(ApiNotifier.new);

class SessionNotifier extends Notifier<SessionState> {
  @override
  SessionState build() => SessionState(
    serverUrl: ref.read(prefsProvider).getString(_kServer) ?? '',
  );

  /// Tries to resume an existing session (browser cookie). Anything but a valid session → anonymous.
  Future<void> restore() async {
    if (!ref.read(sameOriginProvider)) {
      final uri = Uri.tryParse(state.serverUrl);
      if (uri == null ||
          !{'http', 'https'}.contains(uri.scheme) ||
          uri.host.isEmpty) {
        state = state.copyWith(
          status: SessionStatus.anonymous,
          clearAdmin: true,
        );
        return;
      }
    }
    try {
      final admin = await ref.read(apiProvider).me();
      state = admin == null
          ? state.copyWith(status: SessionStatus.anonymous, clearAdmin: true)
          : state.copyWith(status: SessionStatus.authenticated, admin: admin);
    } on Exception {
      state = state.copyWith(status: SessionStatus.anonymous, clearAdmin: true);
    }
  }

  /// Returns an error message, or null on success.
  Future<String?> login(
    String name,
    String password, {
    String? serverUrl,
  }) async {
    var url = serverUrl?.trim() ?? state.serverUrl;
    if (!ref.read(sameOriginProvider)) {
      final parsed = Uri.tryParse(url);
      if (parsed == null ||
          !{'http', 'https'}.contains(parsed.scheme) ||
          parsed.host.isEmpty) {
        return '请输入完整的服务器地址，例如 https://frp.example.com';
      }
      url = url.replaceAll(RegExp(r'/+$'), '');
      ref.read(apiProvider.notifier).useServer(url);
      await ref.read(prefsProvider).setString(_kServer, url);
    }
    try {
      final admin = await ref.read(apiProvider).login(name.trim(), password);
      state = SessionState(
        status: SessionStatus.authenticated,
        admin: admin,
        serverUrl: url,
      );
      return null;
    } on ApiException catch (e) {
      return e.message;
    }
  }

  Future<void> logout() async {
    try {
      await ref.read(apiProvider).logout();
    } on Exception {
      // The session is gone locally either way.
    }
    state = state.copyWith(status: SessionStatus.anonymous, clearAdmin: true);
  }

  /// The server rejected our session (expired, revoked, admin disabled).
  void expire() {
    if (state.status == SessionStatus.authenticated) {
      state = state.copyWith(status: SessionStatus.anonymous, clearAdmin: true);
    }
  }
}

final sessionProvider = NotifierProvider<SessionNotifier, SessionState>(
  SessionNotifier.new,
);

// ───────────── navigation / preferences ─────────────

enum Section { overview, nodes, clients, tunnels, audit, settings }

class SectionNotifier extends Notifier<Section> {
  @override
  Section build() => Section.overview;
  void select(Section s) => state = s;
}

final sectionProvider = NotifierProvider<SectionNotifier, Section>(
  SectionNotifier.new,
);

class SelectionNotifier extends Notifier<String?> {
  @override
  String? build() => null;
  void select(String? id) => state = id;
}

/// Selected client id (clients section) / settings page / node name.
final selectedClientProvider = NotifierProvider<SelectionNotifier, String?>(
  SelectionNotifier.new,
);
final selectedSettingProvider = NotifierProvider<SelectionNotifier, String?>(
  SelectionNotifier.new,
);

/// 节点区选中的节点 ID（内置节点为空串，`null` = 未选中、显示列表）。
final selectedNodeProvider = NotifierProvider<SelectionNotifier, String?>(
  SelectionNotifier.new,
);

class TunnelFilterNotifier extends Notifier<TunnelState?> {
  @override
  TunnelState? build() => null;
  void select(TunnelState? s) => state = s;
}

final tunnelFilterProvider =
    NotifierProvider<TunnelFilterNotifier, TunnelState?>(
      TunnelFilterNotifier.new,
    );

class ThemeModeNotifier extends Notifier<ThemeMode> {
  @override
  ThemeMode build() => switch (ref.read(prefsProvider).getString(_kTheme)) {
    'light' => ThemeMode.light,
    'dark' => ThemeMode.dark,
    _ => ThemeMode.system,
  };

  Future<void> set(ThemeMode m) async {
    state = m;
    await ref.read(prefsProvider).setString(_kTheme, m.name);
  }
}

final themeModeProvider = NotifierProvider<ThemeModeNotifier, ThemeMode>(
  ThemeModeNotifier.new,
);

class SidebarCollapsedNotifier extends Notifier<bool> {
  @override
  bool build() => ref.read(prefsProvider).getBool('sidebar_collapsed') ?? false;
  Future<void> toggle() async {
    state = !state;
    await ref.read(prefsProvider).setBool('sidebar_collapsed', state);
  }
}

final sidebarCollapsedProvider =
    NotifierProvider<SidebarCollapsedNotifier, bool>(
      SidebarCollapsedNotifier.new,
    );

// ───────────── data ─────────────

FutureProvider<T> _live<T>(Future<T> Function(ApiClient api) load) =>
    FutureProvider<T>((ref) {
      ref.watch(tickProvider);
      return load(ref.watch(apiProvider));
    });

typedef TrafficFilter = ({int hours, String? tunnelId, String? clientId});
final trafficProvider = FutureProvider.autoDispose
    .family<TrafficHistory, TrafficFilter>((ref, filter) {
      ref.watch(tickProvider);
      return ref
          .watch(apiProvider)
          .traffic(
            hours: filter.hours,
            tunnelId: filter.tunnelId,
            clientId: filter.clientId,
          );
    });

final overviewProvider = _live((a) => a.overview());
final nodeProvider = _live((a) => a.node());
final nodesProvider = _live((a) => a.nodes());
final clientsProvider = _live((a) => a.clients());
final tunnelsProvider = _live((a) => a.tunnels());
final tokensProvider = _live((a) => a.enrollTokens());
final auditProvider = _live((a) => a.audit());
final controllerLogsProvider = _live((a) => a.controllerLogs());
final apiTokensProvider = _live((a) => a.apiTokens());
final adminsProvider = _live((a) => a.admins());

/// Mutations: call the API, then refresh whatever the change touches. Errors propagate to the caller (the UI
/// shows them as toasts).
class Actions {
  Actions(this.ref);
  final Ref ref;
  ApiClient get _api => ref.read(apiProvider);

  void _refresh() {
    for (final p in [
      overviewProvider,
      nodesProvider,
      clientsProvider,
      tunnelsProvider,
      tokensProvider,
      auditProvider,
    ]) {
      ref.invalidate(p);
    }
  }

  Future<T> _do<T>(Future<T> Function() f) async {
    final r = await f();
    _refresh();
    return r;
  }

  Future<ClientDevice> renameClient(String id, String name, String reason) =>
      _do(() => _api.patchClient(id, name: name, reason: reason));
  Future<ClientDevice> setClientTags(
    String id,
    List<String> tags,
    String reason,
  ) => _do(() => _api.patchClient(id, tags: tags, reason: reason));
  Future<void> restartClient(String id, String reason) =>
      _do(() => _api.restartClient(id, reason: reason));
  Future<ClientDevice> setClientDisabled(
    String id,
    bool disabled,
    String reason,
  ) => _do(() => _api.patchClient(id, disabled: disabled, reason: reason));
  Future<void> kickClient(String id, String reason) =>
      _do(() => _api.kickClient(id, reason: reason));
  Future<void> deleteClient(String id, String reason) =>
      _do(() => _api.deleteClient(id, reason: reason));

  Future<Tunnel> createTunnel({
    required String clientId,
    required String name,
    required int localPort,
    String localIp = '127.0.0.1',
    int remotePort = 0,
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
    String lbGroup = '',
    String lbGroupKey = '',
    HealthCheck? healthCheck,
    required String reason,
  }) => _do(
    () => _api.createTunnel(
      clientId: clientId,
      name: name,
      localPort: localPort,
      localIp: localIp,
      remotePort: remotePort,
      kind: kind,
      customDomains: customDomains,
      subdomain: subdomain,
      hostHeaderRewrite: hostHeaderRewrite,
      locations: locations,
      httpUser: httpUser,
      httpPassword: httpPassword,
      routeByHttpUser: routeByHttpUser,
      requestHeaders: requestHeaders,
      responseHeaders: responseHeaders,
      proxyProtocol: proxyProtocol,
      bandwidthBps: bandwidthBps,
      bandwidthServer: bandwidthServer,
      lbGroup: lbGroup,
      lbGroupKey: lbGroupKey,
      healthCheck: healthCheck,
      reason: reason,
    ),
  );

  Future<Tunnel> updateTunnel(
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
    String? lbGroup,
    String? lbGroupKey,
    HealthCheck? healthCheck,
    required String reason,
  }) => _do(
    () => _api.patchTunnel(
      id,
      name: name,
      localIp: localIp,
      localPort: localPort,
      remotePort: remotePort,
      enabled: enabled,
      customDomains: customDomains,
      subdomain: subdomain,
      hostHeaderRewrite: hostHeaderRewrite,
      locations: locations,
      httpUser: httpUser,
      httpPassword: httpPassword,
      routeByHttpUser: routeByHttpUser,
      requestHeaders: requestHeaders,
      responseHeaders: responseHeaders,
      proxyProtocol: proxyProtocol,
      bandwidthBps: bandwidthBps,
      bandwidthServer: bandwidthServer,
      lbGroup: lbGroup,
      lbGroupKey: lbGroupKey,
      healthCheck: healthCheck,
      reason: reason,
    ),
  );

  Future<void> batchTunnels(List<String> ids, bool enabled, String reason) =>
      _do(() => _api.batchTunnels(ids, enabled, reason));

  Future<void> deleteTunnel(String id, String reason) =>
      _do(() => _api.deleteTunnel(id, reason: reason));

  Future<EnrollResult> createToken(
    String name,
    int ttlMinutes,
    String reason, {
    String kind = 'client',
    String nodeId = '',
  }) => _do(
    () => _api.createEnrollToken(
      name: name,
      ttlMinutes: ttlMinutes,
      kind: kind,
      nodeId: nodeId,
      reason: reason,
    ),
  );

  Future<NodeInfo> renameNode(String id, String name, String reason) =>
      _do(() => _api.patchNode(id, name: name, reason: reason));
  Future<NodeInfo> setNodeDisabled(String id, bool disabled, String reason) =>
      _do(() => _api.patchNode(id, disabled: disabled, reason: reason));
  Future<void> deleteNode(String id, String reason) =>
      _do(() => _api.deleteNode(id, reason: reason));
  Future<void> revokeToken(String id, String reason) =>
      _do(() => _api.revokeEnrollToken(id, reason: reason));
}

final actionsProvider = Provider<Actions>(Actions.new);
