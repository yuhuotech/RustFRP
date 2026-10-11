import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:http/http.dart' as http;

const _fontFiles = ['NotoSansSC-Regular.ttf', 'NotoSansSC-Bold.ttf'];

/// Web: register the bundled Chinese font before the first frame (see web/fonts/README.md).
/// Never fails start-up: without it the text falls back to the engine's default fonts.
Future<void> loadWebFonts({http.Client? client, bool? web}) async {
  if (!(web ?? kIsWeb)) return;
  final c = client ?? http.Client();
  Future<ByteData?> fetch(String f) async {
    try {
      final r = await c.get(Uri.base.resolve('fonts/$f'));
      if (r.statusCode != 200) throw StateError('font $f: ${r.statusCode}');
      return ByteData.sublistView(r.bodyBytes);
    } on Object catch (e) {
      debugPrint('web font $f unavailable: $e');
      return null;
    }
  }

  final data = (await Future.wait(_fontFiles.map(fetch))).nonNulls.toList();
  if (data.isEmpty) return;
  try {
    final loader = FontLoader('NotoSansSC');
    for (final d in data) {
      loader.addFont(Future.value(d));
    }
    await loader.load().timeout(const Duration(seconds: 20));
  } on Object catch (e) {
    debugPrint('web fonts could not be registered: $e');
  }
}
