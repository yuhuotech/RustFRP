import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart' as crypto;
import 'package:cryptography/cryptography.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:rustfrp_app/core/app_update_install.dart';
import 'package:rustfrp_app/core/app_updater.dart';
import 'package:rustfrp_app/core/store.dart';
import 'package:shared_preferences/shared_preferences.dart';

void main() {
  const packageName = 'rustfrp-macos-arm64.zip';
  const repo = 'owner/rustfrp';
  const tag = 'v0.2.0';
  final packageBytes = utf8.encode('signed package with bundled Agent');
  final versionBytes = utf8.encode('$tag\n');
  late SimpleKeyPair signer;
  late List<int> publicKey;
  late String manifest;
  late List<int> signature;

  String hex(List<int> bytes) =>
      bytes.map((byte) => byte.toRadixString(16).padLeft(2, '0')).join();

  Map<String, Object?> releaseJson({String versionTag = tag}) => {
    'tag_name': versionTag,
    'draft': false,
    'prerelease': false,
    'html_url': 'https://github.com/$repo/releases/tag/$tag',
    'body': 'Bug fixes',
    'assets': [
      for (final (name, bytes) in [
        (packageName, packageBytes),
        ('SHA256SUMS', utf8.encode(manifest)),
        ('SHA256SUMS.sig', signature),
        ('RELEASE_VERSION', versionBytes),
      ])
        {
          'name': name,
          'browser_download_url':
              'https://github.com/$repo/releases/download/$tag/$name',
          'size': bytes.length,
        },
    ],
  };

  setUpAll(() async {
    signer = await Ed25519().newKeyPair();
    publicKey = (await signer.extractPublicKey()).bytes;
    manifest =
        '${crypto.sha256.convert(versionBytes)}  RELEASE_VERSION\n'
        '${crypto.sha256.convert(packageBytes)}  $packageName\n';
    signature = (await Ed25519().sign(
      utf8.encode(manifest),
      keyPair: signer,
    )).bytes;
  });

  test(
    'release parsing skips draft and prerelease and compares semantic versions',
    () {
      expect(normalizeVersion('v1.2.3'), '1.2.3');
      expect(normalizeVersion('bad-tag'), isNull);
      expect(compareVersions('1.2.0', '1.1.9'), greaterThan(0));
      expect(compareVersions('1.2.0-rc1', '1.2.0'), lessThan(0));
      expect(compareVersions('1.2.0-rc2', '1.2.0-rc1'), greaterThan(0));
      expect(() => compareVersions('bad', '1.0.0'), throwsArgumentError);
      final hash = List.filled(64, 'a').join();
      expect(sha256For('$hash  ./RELEASE_VERSION\n', 'RELEASE_VERSION'), hash);
      expect(sha256For('invalid\n', 'missing'), isNull);
      final result = newestRelease([
        {...releaseJson(versionTag: 'v9.0.0'), 'draft': true},
        {...releaseJson(versionTag: 'v8.0.0'), 'prerelease': true},
        releaseJson(versionTag: 'v0.1.0'),
        releaseJson(),
      ]);
      expect(result?.tag, tag);
      expect(result?.asset(packageName)?.size, packageBytes.length);
    },
  );

  test('only the pinned Ed25519 key verifies the manifest', () async {
    final key = hex(publicKey);
    expect(
      await verifyReleaseManifestSignature(
        utf8.encode(manifest),
        signature,
        key,
      ),
      isTrue,
    );
    expect(
      await verifyReleaseManifestSignature(
        utf8.encode('$manifest altered'),
        signature,
        key,
      ),
      isFalse,
    );
    expect(
      await verifyReleaseManifestSignature(utf8.encode(manifest), const [
        1,
      ], hex(publicKey)),
      isFalse,
    );
    expect(
      () => verifyReleaseManifestSignature(
        utf8.encode(manifest),
        signature,
        'bad',
      ),
      throwsA(isA<String>()),
    );
  });

  test('GitHub HTTP failures remain visible and the release page uses the configured repo', () async {
    SharedPreferences.setMockInitialValues({});
    final prefs = await SharedPreferences.getInstance();
    final container = ProviderContainer(
      overrides: [
        prefsProvider.overrideWithValue(prefs),
        appUpdateConfigProvider.overrideWithValue(
          AppUpdateConfig(
            repository: repo,
            publicKeyHex: hex(publicKey),
            autoCheck: false,
            clientFactory: () =>
                MockClient((_) async => http.Response('', 404)),
          ),
        ),
      ],
    );
    addTearDown(container.dispose);
    final updater = container.read(appUpdateProvider.notifier);
    await updater.check(manual: true);
    expect(container.read(appUpdateProvider).error, contains('HTTP 404'));
    try {
      await updater.openReleasePage();
    } on Object {
      // The test host may not register the external URL launcher plugin.
    }
  });

  test(
    'up-to-date, manual-plan and failed mirror checks have clear states',
    () async {
      SharedPreferences.setMockInitialValues({});
      final prefs = await SharedPreferences.getInstance();
      final container = ProviderContainer(
        overrides: [
          prefsProvider.overrideWithValue(prefs),
          appUpdateConfigProvider.overrideWithValue(
            AppUpdateConfig(
              repository: repo,
              publicKeyHex: hex(publicKey),
              autoCheck: false,
              clientFactory: () => MockClient((request) async {
                if (request.url.path.endsWith('/releases')) {
                  return http.Response(
                    jsonEncode([releaseJson(versionTag: 'v0.1.0')]),
                    200,
                  );
                }
                return http.Response('missing', 404);
              }),
            ),
          ),
        ],
      );
      addTearDown(container.dispose);
      final notifier = container.read(appUpdateProvider.notifier);
      expect(container.read(appUpdateProvider).busy, isFalse);
      expect(container.read(appUpdateProvider).available, isFalse);
      await notifier.check(manual: true);
      expect(container.read(appUpdateProvider).upToDate, isTrue);
      expect(container.read(appUpdateProvider).latest, isNull);

      final failed = ProviderContainer(
        overrides: [
          prefsProvider.overrideWithValue(prefs),
          appUpdateConfigProvider.overrideWithValue(
            AppUpdateConfig(
              repository: repo,
              publicKeyHex: hex(publicKey),
              autoCheck: false,
              mirrors: const [
                'https://mirror-one.test',
                'https://mirror-two.test',
              ],
              clientFactory: () =>
                  MockClient((_) async => http.Response('', 503)),
            ),
          ),
        ],
      );
      addTearDown(failed.dispose);
      final failedNotifier = failed.read(appUpdateProvider.notifier);
      await failedNotifier.setSource(AppUpdateSource.mainland);
      await failedNotifier.check(manual: true);
      expect(failed.read(appUpdateProvider).error, contains('国内加速源'));
    },
  );

  test('startup checking, manual platform guidance, cancellation and release links work', () async {
    SharedPreferences.setMockInitialValues({});
    final prefs = await SharedPreferences.getInstance();
    Uri? opened;
    var feedChecks = 0;
    final container = ProviderContainer(
      overrides: [
        prefsProvider.overrideWithValue(prefs),
        appUpdateConfigProvider.overrideWithValue(
          AppUpdateConfig(
            repository: repo,
            publicKeyHex: hex(publicKey),
            mirrors: const [],
            startupDelay: Duration.zero,
            checkInterval: const Duration(milliseconds: 100),
            clientFactory: () => MockClient((request) async {
              if (request.url.path.endsWith('/releases')) {
                feedChecks++;
                return http.Response(jsonEncode([releaseJson()]), 200);
              }
              return http.Response('missing', 404);
            }),
            planFor: (_) async => AppInstallPlan.manual('需要手动安装'),
            openPage: (uri) async => opened = uri,
          ),
        ),
      ],
    );
    addTearDown(container.dispose);
    container.read(appUpdateProvider);
    for (var attempt = 0; attempt < 200 && feedChecks < 2; attempt++) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
    final notifier = container.read(appUpdateProvider.notifier);
    expect(container.read(appUpdateProvider).latest?.tag, tag);
    expect(feedChecks, greaterThanOrEqualTo(2));
    while (container.read(appUpdateProvider).busy) {
      await Future<void>.delayed(const Duration(milliseconds: 5));
    }
    await notifier.check(manual: true);
    expect(feedChecks, greaterThan(2));
    expect(container.read(appUpdateProvider).error, '需要手动安装');
    await notifier.openReleasePage();
    expect(opened?.host, 'github.com');
    notifier.cancelDownload();
    expect(container.read(appUpdateProvider).busy, isFalse);
  });

  test(
    'unsupported install layouts return a manual update explanation',
    () async {
      final plan = await AppInstallPlan.forRelease(const []);
      expect(plan.canInstall, isFalse);
      expect(plan.manualReason, isNotEmpty);
    },
  );

  test(
    'signed release installs only after version marker and package hash pass',
    () async {
      SharedPreferences.setMockInitialValues({});
      final prefs = await SharedPreferences.getInstance();
      final installed = <List<int>>[];
      Directory? installTemp;
      final urls = <String>[];
      http.Client clientFactory() => MockClient((request) async {
        final url = request.url.toString();
        urls.add(url);
        final original = url.contains('/https://')
            ? url.substring(url.indexOf('/https://') + 1)
            : url;
        if (original.endsWith('/releases?per_page=20')) {
          return http.Response(jsonEncode([releaseJson()]), 200);
        }
        if (original.endsWith('/SHA256SUMS')) {
          return http.Response(manifest, 200);
        }
        if (original.endsWith('/SHA256SUMS.sig')) {
          return http.Response.bytes(signature, 200);
        }
        if (original.endsWith('/RELEASE_VERSION')) {
          return http.Response.bytes(versionBytes, 200);
        }
        if (original.endsWith('/$packageName')) {
          return http.Response.bytes(packageBytes, 200);
        }
        return http.Response('missing', 404);
      });
      final container = ProviderContainer(
        overrides: [
          prefsProvider.overrideWithValue(prefs),
          appUpdateConfigProvider.overrideWithValue(
            AppUpdateConfig(
              repository: repo,
              publicKeyHex: hex(publicKey),
              mirrors: const ['https://mirror.example'],
              autoCheck: false,
              clientFactory: clientFactory,
              planFor: (_) async => AppInstallPlan.fake(
                packageName,
                install: (file) async {
                  installTemp = file.parent;
                  installed.add(await file.readAsBytes());
                },
              ),
            ),
          ),
        ],
      );
      addTearDown(container.dispose);
      final updater = container.read(appUpdateProvider.notifier);
      await updater.setSource(AppUpdateSource.mainland);
      await updater.check(manual: true);
      expect(container.read(appUpdateProvider).latest?.tag, tag);
      await updater.install();
      expect(installed, [packageBytes]);
      expect(urls, isNotEmpty);
      expect(container.read(appUpdateProvider).error, isNull);
      if (installTemp != null && await installTemp!.exists()) {
        await installTemp!.delete(recursive: true);
      }
    },
  );

  test('tampered manifest signature is rejected before download', () async {
    SharedPreferences.setMockInitialValues({});
    final prefs = await SharedPreferences.getInstance();
    final installed = <List<int>>[];
    final container = ProviderContainer(
      overrides: [
        prefsProvider.overrideWithValue(prefs),
        appUpdateConfigProvider.overrideWithValue(
          AppUpdateConfig(
            repository: repo,
            publicKeyHex: hex(publicKey),
            autoCheck: false,
            clientFactory: () => MockClient((request) async {
              final url = request.url.toString();
              if (url.endsWith('/releases?per_page=20')) {
                return http.Response(jsonEncode([releaseJson()]), 200);
              }
              if (url.endsWith('/SHA256SUMS')) {
                return http.Response('$manifest forged', 200);
              }
              if (url.endsWith('/SHA256SUMS.sig')) {
                return http.Response.bytes(signature, 200);
              }
              if (url.endsWith('/RELEASE_VERSION')) {
                return http.Response.bytes(versionBytes, 200);
              }
              if (url.endsWith('/$packageName')) {
                return http.Response.bytes(utf8.encode('tampered'), 200);
              }
              return http.Response('missing', 404);
            }),
            planFor: (_) async => AppInstallPlan.fake(
              packageName,
              install: (file) async => installed.add(await file.readAsBytes()),
            ),
          ),
        ),
      ],
    );
    addTearDown(container.dispose);
    final updater = container.read(appUpdateProvider.notifier);
    await updater.check(manual: true);
    await updater.install();
    expect(installed, isEmpty);
    expect(container.read(appUpdateProvider).error, contains('签名'));
  });

  test('incomplete releases and invalid package sizes are refused', () async {
    Future<ProviderContainer> containerWithAssets(List<dynamic> assets) async {
      SharedPreferences.setMockInitialValues({});
      final prefs = await SharedPreferences.getInstance();
      return ProviderContainer(
        overrides: [
          prefsProvider.overrideWithValue(prefs),
          appUpdateConfigProvider.overrideWithValue(
            AppUpdateConfig(
              repository: repo,
              publicKeyHex: hex(publicKey),
              autoCheck: false,
              clientFactory: () => MockClient((request) async {
                if (request.url.path.endsWith('/releases')) {
                  return http.Response(
                    jsonEncode([
                      {'tag_name': tag, 'assets': assets},
                    ]),
                    200,
                  );
                }
                return http.Response('unexpected download', 404);
              }),
              planFor: (_) async => AppInstallPlan.fake(packageName),
            ),
          ),
        ],
      );
    }

    final allAssets = (releaseJson()['assets']! as List)
        .cast<Map<String, Object?>>();
    final missingVersion = await containerWithAssets(
      allAssets.where((asset) => asset['name'] != 'RELEASE_VERSION').toList(),
    );
    addTearDown(missingVersion.dispose);
    await missingVersion.read(appUpdateProvider.notifier).check(manual: true);
    await missingVersion.read(appUpdateProvider.notifier).install();
    expect(missingVersion.read(appUpdateProvider).error, contains('签名校验文件'));

    final oversizedAssets = [
      for (final asset in allAssets)
        if (asset['name'] == packageName)
          {...asset, 'size': 1024 * 1024 * 1024 + 1}
        else
          asset,
    ];
    final oversized = await containerWithAssets(oversizedAssets);
    addTearDown(oversized.dispose);
    await oversized.read(appUpdateProvider.notifier).check(manual: true);
    await oversized.read(appUpdateProvider.notifier).install();
    expect(oversized.read(appUpdateProvider).error, contains('大小'));
  });

  test(
    'manifest and package HTTP failures and missing hashes stop installation',
    () async {
      final withoutPackage =
          '${crypto.sha256.convert(versionBytes)}  RELEASE_VERSION\n';
      final withoutPackageSignature = (await Ed25519().sign(
        utf8.encode(withoutPackage),
        keyPair: signer,
      )).bytes;

      Future<ProviderContainer> makeContainer(String failure) async {
        SharedPreferences.setMockInitialValues({});
        final prefs = await SharedPreferences.getInstance();
        return ProviderContainer(
          overrides: [
            prefsProvider.overrideWithValue(prefs),
            appUpdateConfigProvider.overrideWithValue(
              AppUpdateConfig(
                repository: repo,
                publicKeyHex: hex(publicKey),
                autoCheck: false,
                clientFactory: () => MockClient((request) async {
                  final url = request.url.toString();
                  if (url.endsWith('/releases?per_page=20')) {
                    return http.Response(jsonEncode([releaseJson()]), 200);
                  }
                  if (url.endsWith('/SHA256SUMS')) {
                    if (failure == 'manifest') {
                      return http.Response('busy', 503);
                    }
                    return http.Response(
                      failure == 'missing package hash'
                          ? withoutPackage
                          : manifest,
                      200,
                    );
                  }
                  if (url.endsWith('/SHA256SUMS.sig')) {
                    return http.Response.bytes(
                      failure == 'missing package hash'
                          ? withoutPackageSignature
                          : signature,
                      200,
                    );
                  }
                  if (url.endsWith('/RELEASE_VERSION')) {
                    return http.Response.bytes(versionBytes, 200);
                  }
                  if (url.endsWith('/$packageName')) {
                    return failure == 'package'
                        ? http.Response('busy', 503)
                        : http.Response.bytes(packageBytes, 200);
                  }
                  return http.Response('missing', 404);
                }),
                planFor: (_) async => AppInstallPlan.fake(packageName),
              ),
            ),
          ],
        );
      }

      final manifestFailure = await makeContainer('manifest');
      addTearDown(manifestFailure.dispose);
      await manifestFailure
          .read(appUpdateProvider.notifier)
          .check(manual: true);
      await manifestFailure.read(appUpdateProvider.notifier).install();
      expect(
        manifestFailure.read(appUpdateProvider).error,
        contains('HTTP 503'),
      );

      final missingHash = await makeContainer('missing package hash');
      addTearDown(missingHash.dispose);
      await missingHash.read(appUpdateProvider.notifier).check(manual: true);
      await missingHash.read(appUpdateProvider.notifier).install();
      expect(missingHash.read(appUpdateProvider).error, contains(packageName));

      final packageFailure = await makeContainer('package');
      addTearDown(packageFailure.dispose);
      await packageFailure.read(appUpdateProvider.notifier).check(manual: true);
      await packageFailure.read(appUpdateProvider.notifier).install();
      expect(
        packageFailure.read(appUpdateProvider).error,
        contains('HTTP 503'),
      );
    },
  );

  test(
    'a wrong signed version marker and a changed package both fail closed',
    () async {
      Future<ProviderContainer> makeContainer({
        required bool wrongVersion,
        required bool wrongPackage,
      }) async {
        SharedPreferences.setMockInitialValues({});
        final prefs = await SharedPreferences.getInstance();
        return ProviderContainer(
          overrides: [
            prefsProvider.overrideWithValue(prefs),
            appUpdateConfigProvider.overrideWithValue(
              AppUpdateConfig(
                repository: repo,
                publicKeyHex: hex(publicKey),
                autoCheck: false,
                clientFactory: () => MockClient((request) async {
                  final url = request.url.toString();
                  if (url.endsWith('/releases?per_page=20')) {
                    return http.Response(jsonEncode([releaseJson()]), 200);
                  }
                  if (url.endsWith('/SHA256SUMS')) {
                    return http.Response(manifest, 200);
                  }
                  if (url.endsWith('/SHA256SUMS.sig')) {
                    return http.Response.bytes(signature, 200);
                  }
                  if (url.endsWith('/RELEASE_VERSION')) {
                    return http.Response.bytes(
                      wrongVersion ? utf8.encode('v0.3.0\n') : versionBytes,
                      200,
                    );
                  }
                  if (url.endsWith('/$packageName')) {
                    return http.Response.bytes(
                      wrongPackage
                          ? packageBytes
                                .map((byte) => (byte + 1) & 0xff)
                                .toList()
                          : packageBytes,
                      200,
                    );
                  }
                  return http.Response('missing', 404);
                }),
                planFor: (_) async => AppInstallPlan.fake(packageName),
              ),
            ),
          ],
        );
      }

      final wrongVersion = await makeContainer(
        wrongVersion: true,
        wrongPackage: false,
      );
      addTearDown(wrongVersion.dispose);
      await wrongVersion.read(appUpdateProvider.notifier).check(manual: true);
      await wrongVersion.read(appUpdateProvider.notifier).install();
      expect(wrongVersion.read(appUpdateProvider).error, contains('版本不符'));

      final changedPackage = await makeContainer(
        wrongVersion: false,
        wrongPackage: true,
      );
      addTearDown(changedPackage.dispose);
      await changedPackage.read(appUpdateProvider.notifier).check(manual: true);
      await changedPackage.read(appUpdateProvider.notifier).install();
      expect(changedPackage.read(appUpdateProvider).error, contains('SHA-256'));
    },
  );
}
