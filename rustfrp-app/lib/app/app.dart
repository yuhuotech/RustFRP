import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../core/app_updater.dart';
import '../core/store.dart';
import '../features/shell/shell.dart';
import '../theme/tokens.dart';

class RustFrpApp extends ConsumerWidget {
  const RustFrpApp({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    if (!kIsWeb) ref.watch(appUpdateProvider.select((state) => state.phase));
    final mode = ref.watch(themeModeProvider);
    return MaterialApp(
      title: 'RustFRP',
      debugShowCheckedModeBanner: false,
      themeMode: mode,
      theme: buildTheme(Brightness.light),
      darkTheme: buildTheme(Brightness.dark),
      home: const RootGate(),
    );
  }
}
