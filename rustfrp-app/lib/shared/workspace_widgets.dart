import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../theme/tokens.dart';
import 'widgets.dart';

/// A small visual anchor for page descriptions and resource names.
class ToneGlyph extends StatelessWidget {
  const ToneGlyph({super.key, required this.icon, required this.color});
  final IconData icon;
  final Color color;
  @override
  Widget build(BuildContext context) => Container(
    width: 32,
    height: 32,
    decoration: BoxDecoration(
      color: color.withValues(alpha: 0.12),
      borderRadius: BorderRadius.circular(Radii.control),
    ),
    child: Icon(icon, size: 17, color: color),
  );
}

class PageIntro extends StatelessWidget {
  const PageIntro({
    super.key,
    required this.icon,
    required this.title,
    required this.description,
    required this.color,
    this.badges = const [],
  });
  final IconData icon;
  final String title, description;
  final Color color;
  final List<Widget> badges;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: 16),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        ToneGlyph(icon: icon, color: color),
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Wrap(
                spacing: 8,
                runSpacing: 6,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  Text(
                    title,
                    style: Type.section.copyWith(color: context.c.text1),
                  ),
                  ...badges,
                ],
              ),
              const SizedBox(height: 4),
              Text(
                description,
                style: Type.body.copyWith(color: context.c.text3),
              ),
            ],
          ),
        ),
      ],
    ),
  );
}

class DetailPanel extends StatelessWidget {
  const DetailPanel({
    super.key,
    required this.title,
    required this.icon,
    required this.color,
    required this.child,
  });
  final String title;
  final IconData icon;
  final Color color;
  final Widget child;
  @override
  Widget build(BuildContext context) => PanelCard(
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            ToneGlyph(icon: icon, color: color),
            const SizedBox(width: 10),
            Expanded(
              child: Text(
                title,
                style: Type.strong.copyWith(color: context.c.text1),
              ),
            ),
          ],
        ),
        const SizedBox(height: 12),
        child,
      ],
    ),
  );
}

class DetailGrid extends StatelessWidget {
  const DetailGrid({super.key, required this.children});
  final List<Widget> children;
  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, box) {
      final columns = box.maxWidth >= 860 ? 2 : 1;
      final width = (box.maxWidth - (columns - 1) * 12) / columns;
      return Wrap(
        spacing: 12,
        runSpacing: 12,
        children: [
          for (final child in children) SizedBox(width: width, child: child),
        ],
      );
    },
  );
}

/// Full values remain available in a tooltip on narrow columns.
class TableText extends StatelessWidget {
  const TableText(
    this.text, {
    super.key,
    this.subtitle,
    this.color,
    this.mono = false,
  });
  final String text;
  final String? subtitle;
  final Color? color;
  final bool mono;
  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    mainAxisSize: MainAxisSize.min,
    children: [
      Tooltip(
        message: text,
        child: Text(
          text,
          maxLines: 2,
          overflow: TextOverflow.ellipsis,
          style: Type.body.copyWith(
            color: color ?? context.c.text1,
            fontFamily: mono ? 'monospace' : null,
            fontFamilyFallback: mono ? Type.monoFallback : null,
          ),
        ),
      ),
      if (subtitle != null) ...[
        const SizedBox(height: 3),
        Tooltip(
          message: subtitle!,
          child: Text(
            subtitle!,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
            style: Type.caption.copyWith(color: context.c.text3),
          ),
        ),
      ],
    ],
  );
}

class ResourceTable extends StatelessWidget {
  const ResourceTable({
    super.key,
    required this.columns,
    required this.rows,
    this.minWidth = 900,
  });
  final List<(String, int)> columns;
  final List<Widget> rows;
  final double minWidth;
  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, box) => SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: SizedBox(
        width: math.max(box.maxWidth, minWidth),
        child: PanelCard(
          padding: EdgeInsets.zero,
          child: Column(
            children: [
              Container(
                padding: const EdgeInsets.symmetric(
                  horizontal: 16,
                  vertical: 10,
                ),
                decoration: BoxDecoration(
                  color: context.c.bgSubtle,
                  borderRadius: const BorderRadius.vertical(
                    top: Radius.circular(Radii.panel),
                  ),
                ),
                child: Row(
                  children: [
                    for (final column in columns)
                      Expanded(
                        flex: column.$2,
                        child: Text(
                          column.$1,
                          style: Type.caption.copyWith(color: context.c.text3),
                        ),
                      ),
                  ],
                ),
              ),
              for (final row in rows) ...[
                Divider(height: 1, color: context.c.border),
                row,
              ],
            ],
          ),
        ),
      ),
    ),
  );
}

class ResourceRow extends StatelessWidget {
  const ResourceRow({
    super.key,
    required this.cells,
    this.onTap,
    this.selected = false,
  });
  final List<(Widget, int)> cells;
  final VoidCallback? onTap;
  final bool selected;
  @override
  Widget build(BuildContext context) => HoverRow(
    onTap: onTap,
    selected: selected,
    radius: 0,
    child: ConstrainedBox(
      constraints: const BoxConstraints(minHeight: 64),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        child: Row(
          children: [
            for (final cell in cells)
              Expanded(
                flex: cell.$2,
                child: Padding(
                  padding: const EdgeInsets.only(right: 12),
                  child: cell.$1,
                ),
              ),
          ],
        ),
      ),
    ),
  );
}
