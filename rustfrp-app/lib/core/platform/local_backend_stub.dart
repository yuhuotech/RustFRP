import '../local_agent.dart';

LocalAgentBackend createLocalBackend() => const UnsupportedLocalBackend();

class UnsupportedLocalBackend implements LocalAgentBackend {
  const UnsupportedLocalBackend();
  @override
  bool get available => false;
  @override
  LocalAgentConfig get defaults => const LocalAgentConfig();
  @override
  Future<LocalAgentStatus?> status(LocalAgentConfig config) async => null;
  @override
  Future<String> exportDiagnostics(LocalAgentConfig config) async =>
      throw UnsupportedError('本机模式仅支持桌面端');
  @override
  Future<List<Map<String, dynamic>>> tunnels(LocalAgentConfig config) async =>
      throw UnsupportedError('本机模式仅支持桌面端');
  @override
  Future<List<String>> logs(LocalAgentConfig config) async =>
      throw UnsupportedError('本机模式仅支持桌面端');
  @override
  Future<void> start(LocalAgentConfig config, String enrollToken) async =>
      throw UnsupportedError('本机模式仅支持桌面端');
  @override
  Future<void> stop(LocalAgentConfig config) async =>
      throw UnsupportedError('本机模式仅支持桌面端');
}
