import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../core/desktop.dart';
import '../core/store.dart';
import '../theme/tokens.dart';
import 'widgets.dart';

class SidebarEntry {
  const SidebarEntry({
    required this.label,
    required this.icon,
    required this.onSelect,
    this.selected = false,
  });
  final String label;
  final IconData icon;
  final VoidCallback onSelect;
  final bool selected;
}

/// One navigation column; resource lists belong to the main workspace.
class AppSidebar extends ConsumerWidget {
  const AppSidebar({
    super.key,
    required this.items,
    required this.bottomItems,
    required this.onHome,
    this.status,
  });
  static const expandedWidth = 160.0;
  static const collapsedWidth = 72.0;
  final List<SidebarEntry> items, bottomItems;
  final VoidCallback onHome;
  final Widget? status;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = context.c;
    final collapsed = ref.watch(sidebarCollapsedProvider);
    return Container(
      key: const Key('app-sidebar'),
      width: collapsed ? collapsedWidth : expandedWidth,
      color: c.bgSidebar,
      child: Column(
        children: [
          SizedBox(height: windowShellActive ? 44 : 24),
          SizedBox(
            height: 28,
            child: collapsed
                ? Center(child: _toggle(ref, collapsed))
                : Padding(
                    padding: const EdgeInsets.only(left: 20, right: 12),
                    child: Row(
                      children: [
                        BrandHomeButton(onPressed: onHome, size: 22),
                        const SizedBox(width: 8),
                        Expanded(
                          child: Text(
                            'RustFRP',
                            style: Type.body.copyWith(color: c.text1),
                          ),
                        ),
                        _toggle(ref, collapsed),
                      ],
                    ),
                  ),
          ),
          const SizedBox(height: 24),
          Expanded(
            child: SingleChildScrollView(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              child: Column(
                children: [
                  for (final item in items)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 6),
                      child: _MenuItem(item: item, collapsed: collapsed),
                    ),
                ],
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Container(height: 1, color: c.border),
          ),
          const SizedBox(height: 8),
          ?status,
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: Column(
              children: [
                for (final item in bottomItems)
                  Padding(
                    padding: const EdgeInsets.only(top: 4),
                    child: _MenuItem(item: item, collapsed: collapsed),
                  ),
              ],
            ),
          ),
          const SizedBox(height: 12),
        ],
      ),
    );
  }

  Widget _toggle(WidgetRef ref, bool collapsed) => Semantics(
    button: true,
    label: collapsed ? '展开菜单' : '收起菜单',
    excludeSemantics: true,
    onTap: () => ref.read(sidebarCollapsedProvider.notifier).toggle(),
    child: LIconButton(
      key: const Key('sidebar-toggle'),
      icon: collapsed ? LucideIcons.chevronsRight : LucideIcons.chevronsLeft,
      tooltip: collapsed ? '展开菜单' : '收起菜单',
      size: 24,
      iconSize: 16,
      onPressed: () => ref.read(sidebarCollapsedProvider.notifier).toggle(),
    ),
  );
}

class _MenuItem extends StatelessWidget {
  const _MenuItem({required this.item, required this.collapsed});
  final SidebarEntry item;
  final bool collapsed;
  @override
  Widget build(BuildContext context) {
    final c = context.c;
    return Tooltip(
      message: item.label,
      child: HoverRow(
        selected: item.selected,
        selectedColor: c.actionSoft,
        selectedBorderColor: c.action.withValues(alpha: 0.45),
        padding: EdgeInsets.symmetric(horizontal: collapsed ? 0 : 12),
        onTap: item.onSelect,
        child: SizedBox(
          height: 40,
          child: Row(
            mainAxisAlignment: collapsed
                ? MainAxisAlignment.center
                : MainAxisAlignment.start,
            children: [
              Icon(
                item.icon,
                size: 18,
                color: switch (item.icon) {
                  LucideIcons.monitor || LucideIcons.settings => c.directText,
                  LucideIcons.scrollText => c.warningText,
                  LucideIcons.waypoints || LucideIcons.server => c.actionText,
                  _ => item.selected ? c.text1 : c.text2,
                },
              ),
              if (!collapsed) ...[
                const SizedBox(width: 12),
                Expanded(
                  child: Text(
                    item.label,
                    style: Type.body.copyWith(
                      color: item.selected ? c.actionText : c.text2,
                      fontWeight: item.selected
                          ? FontWeight.w500
                          : FontWeight.w400,
                    ),
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}
