import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/desktop.dart';
import '../../core/models.dart';
import '../../core/store.dart';
import '../../shared/panel_widgets.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';
import '../audit/audit_view.dart';
import '../auth/login_page.dart';
import '../clients/clients_view.dart';
import '../nodes/nodes_view.dart';
import '../overview/overview_view.dart';
import '../settings/settings_view.dart';
import '../tunnels/tunnels_view.dart';

/// One entry of the navigation rail / bottom bar.
class NavSpec {
  const NavSpec(this.section, this.icon, this.label, {this.minRole = Role.viewer, this.hasList = true});
  final Section section;
  final IconData icon;
  final String label;
  final Role minRole;

  /// Whether the wide layout shows the 280px list column for this section.
  final bool hasList;
}

const navItems = [
  NavSpec(Section.overview, LucideIcons.layoutDashboard, '概览', hasList: false),
  NavSpec(Section.nodes, LucideIcons.server, '节点'),
  NavSpec(Section.clients, LucideIcons.monitor, '客户端'),
  NavSpec(Section.tunnels, LucideIcons.waypoints, '隧道'),
  NavSpec(Section.audit, LucideIcons.scrollText, '审计', minRole: Role.admin, hasList: false),
];

/// Root: decides between splash, login and the app shell.
class RootGate extends ConsumerStatefulWidget {
  const RootGate({super.key});
  @override
  ConsumerState<RootGate> createState() => _RootGateState();
}

class _RootGateState extends ConsumerState<RootGate> {
  @override
  void initState() {
    super.initState();
    // A browser may already hold a valid session cookie.
    Future.microtask(() => ref.read(sessionProvider.notifier).restore());
  }

  @override
  Widget build(BuildContext context) {
    final status = ref.watch(sessionProvider.select((s) => s.status));
    return switch (status) {
      SessionStatus.unknown => Scaffold(backgroundColor: context.c.bgApp, body: const Center(child: BrandLogo(size: 40))),
      SessionStatus.anonymous => const LoginPage(),
      SessionStatus.authenticated => const AppShell(),
    };
  }
}

class AppShell extends ConsumerWidget {
  const AppShell({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final role = ref.watch(sessionProvider.select((s) => s.role));
    final section = ref.watch(sectionProvider);
    final visible = [for (final n in navItems) if (role.atLeast(n.minRole)) n];
    // A role downgrade (or a stale selection) must never leave an unauthorised page open.
    final effective = section == Section.settings || visible.any((n) => n.section == section) ? section : Section.overview;
    return Scaffold(
      backgroundColor: context.c.bgApp,
      body: isNarrow(context)
          ? _NarrowLayout(items: visible, section: effective)
          : _WideLayout(items: visible, section: effective),
    );
  }
}

// ───────────── wide: rail | list | content ─────────────

class _WideLayout extends ConsumerWidget {
  const _WideLayout({required this.items, required this.section});
  final List<NavSpec> items;
  final Section section;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final hasList = section == Section.settings || items.firstWhere((n) => n.section == section).hasList;
    return Row(children: [
      _Rail(items: items, section: section),
      Container(width: 1, color: c.border),
      if (hasList) ...[
        Container(width: 280, color: c.bgSidebar, child: listPaneFor(section)),
        Container(width: 1, color: c.border),
      ],
      Expanded(child: contentFor(section)),
    ]);
  }
}

class _Rail extends ConsumerWidget {
  const _Rail({required this.items, required this.section});
  final List<NavSpec> items;
  final Section section;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final top = (isDesktop && windowShellActive) ? 44.0 : 22.0;
    return Container(
      width: 72,
      color: c.bgSidebar,
      child: Column(children: [
        SizedBox(height: top),
        BrandHomeButton(onPressed: () => ref.read(sectionProvider.notifier).select(Section.overview)),
        const SizedBox(height: 16),
        for (final n in items) ...[
          _NavButton(spec: n, selected: n.section == section),
          const SizedBox(height: 4),
        ],
        const Spacer(),
        const _ConnectionDot(),
        const SizedBox(height: 10),
        _NavButton(spec: const NavSpec(Section.settings, LucideIcons.settings, '设置'), selected: section == Section.settings),
        const SizedBox(height: 16),
      ]),
    );
  }
}

class _NavButton extends ConsumerWidget {
  const _NavButton({required this.spec, required this.selected});
  final NavSpec spec;
  final bool selected;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    return Tooltip(
      message: spec.label,
      child: HoverRow(
        selected: selected,
        onTap: () => ref.read(sectionProvider.notifier).select(spec.section),
        child: SizedBox(
          width: 48,
          height: 32,
          child: Icon(spec.icon, size: 18, color: selected ? c.text1 : c.text2),
        ),
      ),
    );
  }
}

/// Green when the controller answers, red when it does not (derived from the live overview request).
class _ConnectionDot extends ConsumerWidget {
  const _ConnectionDot();
  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final o = ref.watch(overviewProvider);
    final color = o.hasError ? c.danger : (o.hasValue ? c.success : c.warning);
    return Tooltip(
      message: o.hasError ? '无法连接控制器' : (o.hasValue ? '已连接' : '连接中…'),
      child: StatusDot(key: const Key('connection-dot'), color: color),
    );
  }
}

// ───────────── narrow: single column + bottom nav ─────────────

class _NarrowLayout extends ConsumerWidget {
  const _NarrowLayout({required this.items, required this.section});
  final List<NavSpec> items;
  final Section section;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final all = [...items, const NavSpec(Section.settings, LucideIcons.settings, '设置')];
    final Widget page = switch (section) {
      Section.overview => const OverviewView(),
      Section.audit => const AuditView(),
      // Tunnels already is a flat list: show it directly (with its own filter chips).
      Section.tunnels => const TunnelsView(showFilter: true),
      _ => listPaneFor(section, narrow: true),
    };
    return SafeArea(
      child: Column(children: [
        Expanded(child: page),
        Container(
          decoration: BoxDecoration(color: c.bgSidebar, border: Border(top: BorderSide(color: c.border))),
          child: Row(children: [
            for (final n in all)
              Expanded(
                child: GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onTap: () => ref.read(sectionProvider.notifier).select(n.section),
                  child: SizedBox(
                    height: 52,
                    child: Column(mainAxisAlignment: MainAxisAlignment.center, children: [
                      Icon(n.icon, size: 20, color: n.section == section ? c.actionText : c.text3),
                      const SizedBox(height: 2),
                      Text(n.label, style: Type.badge.copyWith(color: n.section == section ? c.actionText : c.text3)),
                    ]),
                  ),
                ),
              ),
          ]),
        ),
      ]),
    );
  }
}

// ───────────── section registry ─────────────

Widget listPaneFor(Section s, {bool narrow = false}) => switch (s) {
      Section.nodes => NodesList(narrow: narrow),
      Section.clients => ClientsList(narrow: narrow),
      Section.tunnels => const TunnelFilterList(),
      Section.settings => SettingsList(narrow: narrow),
      _ => const SizedBox.shrink(),
    };

Widget contentFor(Section s) => switch (s) {
      Section.overview => const OverviewView(),
      Section.nodes => const NodeDetail(),
      Section.clients => const ClientDetailHost(),
      Section.tunnels => const TunnelsView(),
      Section.audit => const AuditView(),
      Section.settings => const SettingsDetailHost(),
    };

/// Pushes [child] as a full page on narrow screens (list → detail).
void pushDetail(BuildContext context, Widget child) {
  Navigator.of(context).push(MaterialPageRoute<void>(builder: (_) => Scaffold(backgroundColor: context.c.bgApp, body: SafeArea(child: child))));
}
