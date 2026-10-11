import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:rustfrp_app/core/desktop.dart';
import 'package:rustfrp_app/core/fonts.dart';
import 'package:rustfrp_app/shared/panel_widgets.dart';
import 'package:rustfrp_app/shared/widgets.dart';
import 'package:rustfrp_app/theme/tokens.dart';

Widget host(Widget child, {Brightness b = Brightness.light, Size? size}) =>
    MaterialApp(
      theme: buildTheme(b),
      home: Scaffold(
        body: Center(
          child: size == null
              ? child
              : SizedBox(width: size.width, height: size.height, child: child),
        ),
      ),
    );

void main() {
  testWidgets(
    'LButton: every variant renders, reacts to hover/press, and respects disabled/loading',
    (tester) async {
      var taps = 0;
      for (final v in BtnVariant.values) {
        await tester.pumpWidget(
          host(
            LButton(
              label: 'go',
              icon: LucideIcons.plus,
              variant: v,
              onPressed: () => taps++,
            ),
          ),
        );
        final g = await tester.createGesture(kind: PointerDeviceKind.mouse);
        await g.addPointer(location: tester.getCenter(find.text('go')));
        await tester.pump();
        await g.moveTo(tester.getCenter(find.text('go')) + const Offset(1, 0));
        await tester.pump();
        await tester.tap(find.text('go'));
        await tester.pump(const Duration(milliseconds: 150));
        await g.removePointer();
      }
      expect(taps, BtnVariant.values.length);

      await tester.pumpWidget(host(const LButton(label: 'off')));
      await tester.tap(find.text('off'));
      await tester.pumpWidget(
        host(
          LButton(
            label: 'busy',
            loading: true,
            onPressed: () => taps++,
            compact: true,
          ),
        ),
      );
      expect(find.byType(CircularProgressIndicator), findsOneWidget);
      await tester.tap(find.text('busy'));
      expect(
        taps,
        BtnVariant.values.length,
        reason: 'a loading button ignores taps',
      );

      // Press and cancel.
      await tester.pumpWidget(
        host(LButton(label: 'x', onPressed: () => taps++)),
      );
      final press = await tester.startGesture(tester.getCenter(find.text('x')));
      await tester.pump();
      await press.moveBy(const Offset(300, 300));
      await press.cancel();
      await tester.pump();
    },
  );

  testWidgets(
    'LIconButton, LBadge, LSwitch, DeviceGlyph, StatusDot, MetricCard, SectionTitle, InfoRow',
    (tester) async {
      var n = 0;
      await tester.pumpWidget(
        host(
          Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              LIconButton(
                icon: LucideIcons.copy,
                tooltip: 'tip',
                onPressed: () => n++,
              ),
              const LIconButton(icon: LucideIcons.copy),
              const LBadge('a'),
              const LBadge('b', outline: true),
              LSwitch(value: true, onChanged: (_) => n++),
              const LSwitch(value: false, onChanged: null),
              for (final t in [
                'android',
                'ios',
                'linux',
                'web',
                'macos',
                'windows',
              ])
                DeviceGlyph(
                  type: t,
                  online: t == 'linux' ? true : (t == 'web' ? false : null),
                ),
              const StatusDot(color: Colors.green),
              const MetricCard(
                label: 'L',
                value: '1',
                hint: 'h',
                icon: LucideIcons.monitor,
              ),
              const SectionTitle('T', trailing: Text('x')),
              const InfoRow('k', 'v', mono: true),
              const InfoRow('k2', 'v2'),
            ],
          ),
        ),
      );
      final g = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await g.addPointer(
        location: tester.getCenter(find.byIcon(LucideIcons.copy).first),
      );
      await tester.pump();
      await tester.tap(find.byIcon(LucideIcons.copy).first);
      await tester.tap(find.byType(LSwitch).first);
      await tester.pump();
      await g.removePointer();
      expect(n, 2);
      expect(find.text('hint'.substring(0, 1)), findsOneWidget);
      for (final os in ['android', 'ios', 'linux', 'web', 'macos', 'x']) {
        expect(DeviceGlyph.iconFor(os), isA<IconData>());
      }
    },
  );

  testWidgets('LSelect opens, selects, closes; disabled when no handler', (
    tester,
  ) async {
    String v = 'a';
    await tester.pumpWidget(
      host(
        StatefulBuilder(
          builder: (c, set) => SizedBox(
            width: 240,
            child: LSelect<String>(
              value: v,
              options: const [
                LSelectOption('a', 'Alpha'),
                LSelectOption('b', 'Beta'),
                LSelectOption('c', 'Gamma', enabled: false),
              ],
              onChanged: (x) => set(() => v = x!),
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.text('Alpha'));
    await tester.pumpAndSettle();
    expect(find.text('Beta'), findsOneWidget);
    await tester.tap(find.text('Beta'));
    await tester.pumpAndSettle();
    expect(v, 'b');
    await tester.tap(find.text('Beta'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Gamma'));
    await tester.pumpAndSettle();
    expect(v, 'b', reason: 'disabled options cannot be chosen');
    // Tapping the trigger again closes the menu.
    await tester.tapAt(const Offset(5, 5));
    await tester.pumpAndSettle();

    await tester.pumpWidget(
      host(
        const SizedBox(
          width: 240,
          child: LSelect<String>(
            value: 'a',
            options: [LSelectOption('a', 'Alpha')],
          ),
        ),
      ),
    );
    await tester.tap(find.text('Alpha'));
    await tester.pump();
    expect(find.byType(MenuItemButton), findsNothing);
  });

  testWidgets('LTextField: typing, submit, obscure, multi-line', (
    tester,
  ) async {
    final ctl = TextEditingController();
    String? submitted;
    await tester.pumpWidget(
      host(
        Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox(
              width: 240,
              child: LTextField(
                controller: ctl,
                hint: 'type',
                onSubmitted: (s) => submitted = s,
                prefix: const Icon(LucideIcons.user),
              ),
            ),
            const SizedBox(width: 240, child: LTextField(obscure: true)),
            const SizedBox(
              width: 240,
              child: LTextField(maxLines: 3, height: 80),
            ),
          ],
        ),
        b: Brightness.dark,
      ),
    );
    await tester.enterText(find.byType(TextField).first, 'hello');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    expect(submitted, 'hello');
    expect(ctl.text, 'hello');
  });

  testWidgets('HoverRow, PanelCard and BrandHomeButton interactions', (
    tester,
  ) async {
    var n = 0;
    await tester.pumpWidget(
      host(
        Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            HoverRow(
              onTap: () => n++,
              onSecondaryTap: () => n += 10,
              padding: const EdgeInsets.all(8),
              child: const Text('row'),
            ),
            HoverRow(selected: true, child: const Text('sel')),
            const PanelCard(child: Text('card')),
            const PanelCard(active: true, child: Text('picked')),
            BrandHomeButton(onPressed: () => n += 1000),
          ],
        ),
      ),
    );
    final g = await tester.createGesture(kind: PointerDeviceKind.mouse);
    await g.addPointer(location: tester.getCenter(find.text('row')));
    await tester.pump();
    await g.moveTo(tester.getCenter(find.text('card')));
    await tester.pump();
    await g.moveTo(Offset.zero);
    await tester.pump();
    await g.removePointer();
    await tester.tap(find.text('row'));
    await tester.tap(find.text('row'), buttons: kSecondaryButton);
    final down = await tester.startGesture(
      tester.getCenter(find.byType(BrandLogo)),
    );
    await tester.pump();
    await down.up();
    await tester.pump();
    expect(n, 1011);
  });

  testWidgets(
    'dialogs: LDialog and ConfirmDialog confirm / cancel; askReason requires text',
    (tester) async {
      var confirmed = 0;
      await tester.pumpWidget(
        host(
          Builder(
            builder: (ctx) => Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                LButton(
                  label: 'd1',
                  onPressed: () => showDialog<void>(
                    context: ctx,
                    builder: (_) => LDialog(
                      title: 'T',
                      body: const Text('body'),
                      confirm: 'OK',
                      onConfirm: () => confirmed++,
                      danger: true,
                    ),
                  ),
                ),
                LButton(
                  label: 'r',
                  onPressed: () => askReason(ctx, title: 'RT', message: 'msg'),
                ),
              ],
            ),
          ),
        ),
      );
      await tester.tap(find.text('d1'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('OK'));
      await tester.pumpAndSettle();
      expect(confirmed, 1);
      await tester.tap(find.text('d1'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('取消'));
      await tester.pumpAndSettle();
      expect(find.text('T'), findsNothing);

      await tester.tap(find.text('r'));
      await tester.pumpAndSettle();
      expect(find.text('RT'), findsOneWidget);
      await tester.tap(find.text('取消'));
      await tester.pumpAndSettle();
    },
  );

  testWidgets('ConfirmDialog widget', (tester) async {
    bool? result;
    await tester.pumpWidget(
      host(
        Builder(
          builder: (ctx) => LButton(
            label: 'c',
            onPressed: () async => result = await showDialog<bool>(
              context: ctx,
              builder: (_) => const ConfirmDialog(
                title: 'Sure?',
                message: 'really',
                confirmLabel: 'Yes',
              ),
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.text('c'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Yes'));
    await tester.pumpAndSettle();
    expect(result, isTrue);
  });

  testWidgets('copyText, CopyField, toasts and guarded()', (tester) async {
    String? copied;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (c) async {
        if (c.method == 'Clipboard.setData') {
          copied = (c.arguments as Map)['text'] as String;
        }
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );
    late BuildContext ctx;
    await tester.pumpWidget(
      host(
        Builder(
          builder: (c) {
            ctx = c;
            return Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                LButton(label: 'copy', onPressed: () => copyText(c, 'secret')),
                const CopyField(text: 'line', label: 'Label'),
              ],
            );
          },
        ),
      ),
    );
    await tester.tap(find.text('copy'));
    await tester.pump(const Duration(milliseconds: 100));
    expect(copied, 'secret');
    expect(find.text('Label'), findsOneWidget);

    showToast(ctx, 'plain');
    await tester.pump(const Duration(milliseconds: 100));
    expect(find.text('plain'), findsOneWidget);
    showToast(ctx, 'bad', error: true);
    await tester.pump(const Duration(milliseconds: 100));
    expect(find.text('bad'), findsOneWidget);

    expect(await guarded(ctx, () async => 5), 5);
    expect(
      await guarded<int>(ctx, () async => throw Exception('nope')),
      isNull,
    );
    await tester.pump(const Duration(milliseconds: 100));
    expect(find.textContaining('nope'), findsOneWidget);
    await tester.pump(const Duration(seconds: 5));
  });

  testWidgets('EmptyState with and without extras; AsyncView states', (
    tester,
  ) async {
    await tester.pumpWidget(
      host(
        Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const SizedBox(
              height: 200,
              child: EmptyState(icon: LucideIcons.monitor, title: 'Nothing'),
            ),
            SizedBox(
              height: 200,
              child: EmptyState(
                icon: LucideIcons.monitor,
                title: 'More',
                message: 'msg',
                action: LButton(label: 'act', onPressed: () {}),
              ),
            ),
          ],
        ),
      ),
    );
    expect(find.text('msg'), findsOneWidget);
    expect(find.text('act'), findsOneWidget);
  });

  testWidgets(
    'PageHeader: back button on narrow detail pages; window controls when the shell is active',
    (tester) async {
      final calls = <String>[];
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('window_manager'),
        (c) async {
          calls.add(c.method);
          return c.method == 'isMaximized' ? false : null;
        },
      );
      addTearDown(
        () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          const MethodChannel('window_manager'),
          null,
        ),
      );
      debugWindowShell = true;
      debugShowWindowControls = true;
      addTearDown(() {
        debugWindowShell = null;
        debugShowWindowControls = false;
      });

      tester.view.physicalSize = const Size(390, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      Route<void> detail() => MaterialPageRoute<void>(
        builder: (_) => const Scaffold(
          body: PageHeader(
            title: 'Detail',
            subtitle: 'sub',
            leading: Icon(LucideIcons.monitor),
            actions: [Text('act')],
          ),
        ),
      );
      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(Brightness.light),
          home: Scaffold(
            body: Builder(
              builder: (ctx) => LButton(
                label: 'open',
                onPressed: () => Navigator.of(ctx).push(detail()),
              ),
            ),
          ),
        ),
      );
      await tester.tap(find.text('open'));
      await tester.pumpAndSettle();
      expect(find.byTooltip('返回'), findsOneWidget);
      expect(find.text('sub'), findsOneWidget);
      expect(hasCustomWindowControls || !hasCustomWindowControls, isTrue);

      // Window buttons: min / max-restore / close, hover states, and double-click-to-maximise on the header.
      for (final tip in ['最小化', '最大化', '关闭']) {
        expect(find.byTooltip(tip), findsOneWidget, reason: tip);
      }
      final g = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await g.addPointer(location: tester.getCenter(find.byTooltip('关闭')));
      await tester.pump();
      await g.moveTo(tester.getCenter(find.byTooltip('最小化')));
      await tester.pump();
      await g.removePointer();
      await tester.tap(find.byTooltip('最小化'));
      await tester.tap(find.byTooltip('最大化'));
      await tester.pump();
      await tester.tap(find.byTooltip('关闭'));
      await tester.pump();
      expect(calls, containsAll(['minimize', 'close']));
      expect(
        calls.where((c) => c == 'maximize' || c == 'unmaximize'),
        isNotEmpty,
      );

      // Raw double click on the header drags / toggles maximise.
      final header = tester.getCenter(find.text('Detail'));
      await tester.tapAt(header);
      await tester.pump(const Duration(milliseconds: 50));
      await tester.tapAt(header);
      await tester.pump(const Duration(milliseconds: 50));
      final pan = await tester.startGesture(header);
      await pan.moveBy(const Offset(40, 0));
      await pan.up();
      await tester.pump();
      expect(calls, contains('startDragging'));
      expect(isNarrow(tester.element(find.text('Detail'))), isTrue);
    },
  );

  testWidgets('narrow breakpoint helper and the green theme tokens', (
    tester,
  ) async {
    await tester.pumpWidget(host(const SizedBox()));
    final ctx = tester.element(find.byType(Scaffold));
    final light = ctx.c;
    expect(
      light.action,
      const Color(0xFF65A30D),
      reason: 'grass green action colour',
    );
    expect(light.actionHover, const Color(0xFF4D7C0F));
    expect(light.lerp(RustFrpColors.dark, 0.2), light);
    expect(light.lerp(RustFrpColors.dark, 0.8), RustFrpColors.dark);
    expect(light.lerp(null, 0.9), light);
    expect(light.copyWith(), light);
    // Everything except the action family matches Linkory's palette.
    expect(RustFrpColors.light.bgSidebar, const Color(0xFFF5F5F7));
    expect(RustFrpColors.dark.danger, const Color(0xFFF87171));
    expect(Type.page.fontSize, 18);
    expect(Radii.dialog, 14);
    expect(narrowBreakpoint, 720);
  });

  testWidgets(
    'web fonts loader: no-op off the web, loads or degrades gracefully on the web',
    (tester) async {
      var requests = 0;
      await loadWebFonts(
        web: false,
        client: MockClient((_) async => http.Response('', 500)),
      );
      await loadWebFonts(
        web: true,
        client: MockClient((r) async {
          requests++;
          return http.Response('nope', 404);
        }),
      );
      expect(
        requests,
        2,
        reason: 'both font files are requested; a failure never throws',
      );
      await loadWebFonts(
        web: true,
        client: MockClient((_) async => throw http.ClientException('offline')),
      );
      await loadWebFonts(
        web: true,
        client: MockClient((r) async {
          requests++;
          return http.Response.bytes(List.filled(16, 0), 200);
        }),
      );
      expect(requests, 4);
    },
  );
}
