import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:crypto/crypto.dart' as crypto;
import 'package:cryptography/cryptography.dart' as crypt;
import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:http/http.dart' as http;
import 'package:url_launcher/url_launcher.dart';

import 'app_update_install.dart';
import 'app_version.dart';
import 'store.dart';

const _releaseRepository = String.fromEnvironment('RUSTFRP_GITHUB_REPOSITORY');
const _releasePublicKeyHex = String.fromEnvironment(
  'RUSTFRP_RELEASE_PUBLIC_KEY_HEX',
);
const _releaseMirrors = String.fromEnvironment(
  'RUSTFRP_UPDATE_MIRRORS',
  defaultValue: 'https://gh-proxy.com,https://ghfast.top',
);
const _maxFeedBytes = 2 * 1024 * 1024;
const _maxManifestBytes = 1024 * 1024;
const _maxSignatureBytes = 1024;
const _maxPackageBytes = 1024 * 1024 * 1024;

enum AppUpdateSource { github, mainland }

enum AppUpdatePhase { idle, checking, downloading, installing }

class AppReleaseAsset {
  const AppReleaseAsset(this.name, this.url, this.size);
  final String name, url;
  final int size;
}

class AppRelease {
  const AppRelease({
    required this.tag,
    required this.version,
    required this.notes,
    required this.pageUrl,
    required this.assets,
  });
  final String tag, version, notes, pageUrl;
  final List<AppReleaseAsset> assets;

  AppReleaseAsset? asset(String name) =>
      assets.where((item) => item.name == name).firstOrNull;
}

class AppUpdateState {
  const AppUpdateState({
    this.phase = AppUpdatePhase.idle,
    this.latest,
    this.plan,
    this.progress = 0,
    this.error,
    this.upToDate = false,
    this.source = AppUpdateSource.github,
    this.mirrorUsed,
    this.lastChecked,
  });
  final AppUpdatePhase phase;
  final AppRelease? latest;
  final AppInstallPlan? plan;
  final double progress;
  final String? error;
  final bool upToDate;
  final AppUpdateSource source;
  final String? mirrorUsed;
  final DateTime? lastChecked;

  bool get busy => phase != AppUpdatePhase.idle;
  bool get available => latest != null;
  AppUpdateState copyWith({
    AppUpdatePhase? phase,
    Object? latest = _keep,
    Object? plan = _keep,
    double? progress,
    Object? error = _keep,
    bool? upToDate,
    AppUpdateSource? source,
    Object? mirrorUsed = _keep,
    Object? lastChecked = _keep,
  }) => AppUpdateState(
    phase: phase ?? this.phase,
    latest: identical(latest, _keep) ? this.latest : latest as AppRelease?,
    plan: identical(plan, _keep) ? this.plan : plan as AppInstallPlan?,
    progress: progress ?? this.progress,
    error: identical(error, _keep) ? this.error : error as String?,
    upToDate: upToDate ?? this.upToDate,
    source: source ?? this.source,
    mirrorUsed: identical(mirrorUsed, _keep)
        ? this.mirrorUsed
        : mirrorUsed as String?,
    lastChecked: identical(lastChecked, _keep)
        ? this.lastChecked
        : lastChecked as DateTime?,
  );

  static const _keep = Object();
}

class AppUpdateConfig {
  const AppUpdateConfig({
    this.repository = _releaseRepository,
    this.publicKeyHex = _releasePublicKeyHex,
    this.mirrors = const ['https://gh-proxy.com', 'https://ghfast.top'],
    this.planFor = AppInstallPlan.forRelease,
    this.clientFactory = http.Client.new,
    this.autoCheck = true,
    this.startupDelay = const Duration(seconds: 12),
    this.checkInterval = const Duration(hours: 6),
    this.openPage = _openPage,
  });
  final String repository, publicKeyHex;
  final List<String> mirrors;
  final Future<AppInstallPlan> Function(List<String>) planFor;
  final http.Client Function() clientFactory;
  final bool autoCheck;
  final Duration startupDelay, checkInterval;
  final Future<void> Function(Uri) openPage;
}

Future<void> _openPage(Uri uri) =>
    launchUrl(uri, mode: LaunchMode.externalApplication);

final appUpdateConfigProvider = Provider<AppUpdateConfig>((ref) {
  final mirrors = _releaseMirrors
      .split(',')
      .map((item) => item.trim())
      .where((item) => item.isNotEmpty)
      .toList(growable: false);
  return AppUpdateConfig(mirrors: mirrors);
});

final appUpdateProvider = NotifierProvider<AppUpdateNotifier, AppUpdateState>(
  AppUpdateNotifier.new,
);

/// The app updater verifies the signed release manifest before it writes a package to disk.
class AppUpdateNotifier extends Notifier<AppUpdateState> {
  Timer? _startupCheck, _periodicCheck;
  http.Client? _downloadClient;

  @override
  AppUpdateState build() {
    ref.onDispose(() {
      _startupCheck?.cancel();
      _periodicCheck?.cancel();
      _downloadClient?.close();
    });
    final saved = ref.read(prefsProvider).getString('rustfrp_update_source');
    final state = AppUpdateState(
      source: saved == 'mainland'
          ? AppUpdateSource.mainland
          : AppUpdateSource.github,
    );
    final config = ref.read(appUpdateConfigProvider);
    if (!kIsWeb &&
        config.autoCheck &&
        config.repository.isNotEmpty &&
        config.publicKeyHex.isNotEmpty) {
      _startupCheck = Timer(config.startupDelay, () {
        if (ref.mounted) unawaited(check());
      });
      _periodicCheck = Timer.periodic(config.checkInterval, (_) {
        if (ref.mounted) unawaited(check());
      });
    }
    return state;
  }

  AppUpdateConfig get _config => ref.read(appUpdateConfigProvider);
  bool get canOpenReleasePage => _config.repository.isNotEmpty;

  Future<void> setSource(AppUpdateSource source) async {
    await ref
        .read(prefsProvider)
        .setString('rustfrp_update_source', source.name);
    state = state.copyWith(source: source, mirrorUsed: null, error: null);
  }

  Future<void> check({bool manual = false}) async {
    if (state.busy || kIsWeb) return;
    state = state.copyWith(phase: AppUpdatePhase.checking, error: null);
    try {
      final cfg = _config;
      _validateRepository(cfg.repository);
      _parseHex(cfg.publicKeyHex, 32);
      final feedUrl =
          'https://api.github.com/repos/${cfg.repository}/releases?per_page=20';
      final feed = await _getBytes(
        feedUrl,
        headers: {
          'Accept': 'application/vnd.github+json',
          'User-Agent': 'rustfrp/$rustfrpAppVersion',
        },
        maxBytes: _maxFeedBytes,
      );
      final release = newestRelease(jsonDecode(utf8.decode(feed)) as List);
      final now = DateTime.now();
      if (release == null ||
          compareVersions(release.version, rustfrpAppVersion) <= 0) {
        state = state.copyWith(
          phase: AppUpdatePhase.idle,
          latest: null,
          plan: null,
          upToDate: true,
          lastChecked: now,
        );
        return;
      }
      final plan = await cfg.planFor(
        release.assets.map((a) => a.name).toList(),
      );
      state = state.copyWith(
        phase: AppUpdatePhase.idle,
        latest: release,
        plan: plan,
        upToDate: false,
        lastChecked: now,
      );
      if (manual && plan.manualReason != null) {
        state = state.copyWith(error: plan.manualReason);
      }
    } on Object catch (error) {
      _downloadClient?.close();
      _downloadClient = null;
      state = state.copyWith(
        phase: AppUpdatePhase.idle,
        error: '检查更新失败：$error',
        upToDate: false,
        lastChecked: DateTime.now(),
      );
    }
  }

  Future<List<int>> _getBytes(
    String url, {
    required Map<String, String> headers,
    required int maxBytes,
  }) async {
    final client = _config.clientFactory();
    _downloadClient = client;
    try {
      final response = await _get(url, client: client, headers: headers);
      final bytes = BytesBuilder(copy: false);
      var total = 0;
      await for (final chunk in response.stream.timeout(
        const Duration(seconds: 90),
      )) {
        total += chunk.length;
        if (total > maxBytes) throw '更新文件超过大小限制';
        bytes.add(chunk);
      }
      if (total == 0) throw '更新文件为空';
      return bytes.takeBytes();
    } finally {
      client.close();
      if (identical(_downloadClient, client)) _downloadClient = null;
    }
  }

  Future<http.StreamedResponse> _get(
    String url, {
    required http.Client client,
    Map<String, String> headers = const {},
  }) async {
    final urls = state.source == AppUpdateSource.github
        ? [url]
        : _config.mirrors.map((mirror) => throughMirror(mirror, url)).toList();
    Object? lastError;
    for (final target in urls) {
      try {
        final response = await client
            .send(
              http.Request('GET', Uri.parse(target))..headers.addAll(headers),
            )
            .timeout(const Duration(seconds: 30));
        if (response.statusCode == 200) {
          if (state.source == AppUpdateSource.mainland) {
            state = state.copyWith(mirrorUsed: Uri.parse(target).origin);
          }
          return response;
        }
        await response.stream.drain<void>();
        lastError = 'HTTP ${response.statusCode}';
      } on Object catch (error) {
        lastError = error;
      }
    }
    throw state.source == AppUpdateSource.mainland
        ? '国内加速源暂时不可用：$lastError'
        : lastError ?? '无法连接 GitHub';
  }

  Future<void> install() async {
    final release = state.latest;
    final plan = state.plan;
    if (release == null || plan == null || !plan.canInstall || state.busy) {
      return;
    }
    final package = release.asset(plan.assetName!);
    final sums = release.asset('SHA256SUMS');
    final signature = release.asset('SHA256SUMS.sig');
    final releaseVersion = release.asset('RELEASE_VERSION');
    if (package == null ||
        sums == null ||
        signature == null ||
        releaseVersion == null) {
      state = state.copyWith(error: 'Release 缺少安装包或签名校验文件');
      return;
    }
    if (package.size <= 0 || package.size > _maxPackageBytes) {
      state = state.copyWith(error: '安装包大小超出允许范围');
      return;
    }
    File? staged;
    Directory? stageDir;
    try {
      state = state.copyWith(
        phase: AppUpdatePhase.downloading,
        progress: 0,
        error: null,
      );
      final manifest = await _getBytes(
        sums.url,
        headers: const {},
        maxBytes: _maxManifestBytes,
      );
      final sig = await _getBytes(
        signature.url,
        headers: const {},
        maxBytes: _maxSignatureBytes,
      );
      final valid = await verifyReleaseManifestSignature(
        manifest,
        sig,
        _config.publicKeyHex,
      );
      if (!valid) throw 'SHA256SUMS 签名无效，已拒绝安装';
      final expected = sha256For(utf8.decode(manifest), package.name);
      if (expected == null) throw '签名清单中没有 ${package.name}';
      final expectedVersion = sha256For(
        utf8.decode(manifest),
        releaseVersion.name,
      );
      if (expectedVersion == null) throw '签名清单没有 Release 版本标记';
      final signedVersion = utf8.decode(
        await _getBytes(releaseVersion.url, headers: const {}, maxBytes: 128),
      );
      if (crypto.sha256.convert(utf8.encode(signedVersion)).toString() !=
              expectedVersion ||
          signedVersion.trim() != release.tag) {
        throw 'Release 标签与签名版本不符，已拒绝安装';
      }
      stageDir = await Directory.systemTemp.createTemp('rustfrp-app-update-');
      staged = File('${stageDir.path}${Platform.pathSeparator}${package.name}');
      final client = _config.clientFactory();
      _downloadClient = client;
      final response = await _get(
        package.url,
        client: client,
        headers: {'User-Agent': 'rustfrp/$rustfrpAppVersion'},
      );
      final sink = staged.openWrite();
      var total = 0;
      try {
        await for (final chunk in response.stream.timeout(
          const Duration(minutes: 5),
        )) {
          total += chunk.length;
          if (total > _maxPackageBytes || total > package.size) {
            throw '安装包超过发布清单中的大小';
          }
          sink.add(chunk);
          state = state.copyWith(
            progress: (total / package.size).clamp(0.0, 1.0),
          );
        }
        await sink.flush();
      } finally {
        await sink.close();
        client.close();
        _downloadClient = null;
      }
      if (total != package.size) throw '安装包下载长度与发布清单不符';
      final actual = (await crypto.sha256.bind(staged.openRead()).first)
          .toString();
      if (actual != expected) throw '安装包 SHA-256 不符，已拒绝安装';
      state = state.copyWith(phase: AppUpdatePhase.installing, progress: 1);
      await Future<void>.delayed(const Duration(milliseconds: 250));
      await plan.install(staged);
      state = state.copyWith(phase: AppUpdatePhase.idle);
    } on Object catch (error) {
      _downloadClient?.close();
      _downloadClient = null;
      if (stageDir != null && await stageDir.exists()) {
        await stageDir.delete(recursive: true);
      }
      state = state.copyWith(phase: AppUpdatePhase.idle, error: '$error');
    }
  }

  void cancelDownload() {
    _downloadClient?.close();
    _downloadClient = null;
    state = state.copyWith(phase: AppUpdatePhase.idle, progress: 0);
  }

  Future<void> openReleasePage() async {
    if (_config.repository.isEmpty) return;
    await _config.openPage(
      Uri.parse('https://github.com/${_config.repository}/releases'),
    );
  }
}

String throughMirror(String mirror, String original) =>
    '${mirror.replaceAll(RegExp(r'/+$'), '')}/$original';

List<int> _parseHex(String value, int bytes) {
  if (value.length != bytes * 2 || !RegExp(r'^[0-9a-fA-F]+$').hasMatch(value)) {
    throw 'Release 公钥配置无效';
  }
  return [
    for (var i = 0; i < value.length; i += 2)
      int.parse(value.substring(i, i + 2), radix: 16),
  ];
}

Future<bool> verifyReleaseManifestSignature(
  List<int> manifest,
  List<int> signature,
  String publicKeyHex,
) async {
  if (signature.length != 64) return false;
  final key = _parseHex(publicKeyHex, 32);
  return crypt.Ed25519().verify(
    manifest,
    signature: crypt.Signature(
      signature,
      publicKey: crypt.SimplePublicKey(key, type: crypt.KeyPairType.ed25519),
    ),
  );
}

void _validateRepository(String repository) {
  if (!RegExp(r'^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$').hasMatch(repository)) {
    throw 'Release 仓库地址未配置或格式错误';
  }
}

AppRelease? newestRelease(List<dynamic> releases) {
  AppRelease? newest;
  for (final item in releases.whereType<Map<String, dynamic>>()) {
    if (item['draft'] == true || item['prerelease'] == true) continue;
    final tag = item['tag_name'];
    if (tag is! String) continue;
    final version = normalizeVersion(tag);
    if (version == null) continue;
    if (newest != null && compareVersions(version, newest.version) <= 0) {
      continue;
    }
    newest = AppRelease(
      tag: tag,
      version: version,
      notes: item['body'] is String ? item['body'] as String : '',
      pageUrl: item['html_url'] is String
          ? item['html_url'] as String
          : 'https://github.com/$_releaseRepository/releases',
      assets: [
        for (final raw
            in (item['assets'] as List? ?? const [])
                .whereType<Map<String, dynamic>>())
          if (raw['name'] is String && raw['browser_download_url'] is String)
            AppReleaseAsset(
              raw['name'] as String,
              raw['browser_download_url'] as String,
              raw['size'] is num ? (raw['size'] as num).toInt() : 0,
            ),
      ],
    );
  }
  return newest;
}

String? normalizeVersion(String input) {
  final match = RegExp(r'^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$')
      .firstMatch(input.trim());
  if (match == null) return null;
  return '${match[1]}.${match[2]}.${match[3]}${match[4] == null ? '' : '-${match[4]}'}';
}

int compareVersions(String left, String right) {
  final a = normalizeVersion(left);
  final b = normalizeVersion(right);
  if (a == null || b == null) throw ArgumentError('invalid semantic version');
  final ax = a.split('-'), bx = b.split('-');
  final ap = ax.first.split('.').map(int.parse).toList();
  final bp = bx.first.split('.').map(int.parse).toList();
  for (var i = 0; i < 3; i++) {
    final difference = ap[i].compareTo(bp[i]);
    if (difference != 0) return difference;
  }
  if (ax.length == 1 || bx.length == 1) {
    return ax.length == bx.length ? 0 : (ax.length == 1 ? 1 : -1);
  }
  return ax[1].compareTo(bx[1]);
}

String? sha256For(String sums, String fileName) {
  for (final line in const LineSplitter().convert(sums)) {
    final match = RegExp(r'^([0-9a-fA-F]{64})\s+\*?(.+)$')
        .firstMatch(line.trim());
    if (match != null &&
        match[2]!.replaceFirst(RegExp(r'^\./'), '') == fileName) {
      return match[1]!.toLowerCase();
    }
  }
  return null;
}
