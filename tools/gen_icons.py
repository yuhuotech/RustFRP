#!/usr/bin/env python3
"""Regenerate every RustFRP brand asset from the canonical SVG.

Dependencies: pip install cairosvg pillow
Run from any directory: python3 tools/gen_icons.py
"""
from io import BytesIO
from pathlib import Path
import copy
import xml.etree.ElementTree as ET

import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
BRAND = ROOT / 'assets/brand'
APP = ROOT / 'rustfrp-app'
SVG = BRAND / 'rustfrp-logo.svg'
NS = '{http://www.w3.org/2000/svg}'
ET.register_namespace('', NS[1:-1])


def render(source, size):
    return Image.open(BytesIO(cairosvg.svg2png(
        bytestring=source, output_width=size * 4, output_height=size * 4,
    ))).convert('RGBA').resize((size, size), Image.Resampling.LANCZOS)


def save_png(image, path):
    path.parent.mkdir(parents=True, exist_ok=True)
    image.save(path, optimize=True)


def main():
    source = SVG.read_bytes()
    for size in (16, 32, 64, 128, 256, 512, 1024):
        image = render(source, size)
        save_png(image, BRAND / f'rustfrp-logo-{size}.png')
        save_png(image, APP / f'macos/Runner/Assets.xcassets/AppIcon.appiconset/app_icon_{size}.png')
        if size in (128, 512):
            save_png(image, APP / f'assets/brand/rustfrp-logo-{size}.png')
    save_png(render(source, 32), APP / 'web/favicon.png')
    tree = ET.fromstring(source)
    background = tree.find(f'{NS}rect')
    background.set('rx', '0')
    mask_mark = tree.find(f'{NS}g')
    tree.remove(mask_mark)
    safe_area = ET.SubElement(tree, f'{NS}g',
                              {'transform': 'translate(76.8 76.8) scale(.85)'})
    safe_area.append(mask_mark)
    full_bleed = ET.tostring(tree)
    for size in (192, 512):
        save_png(render(source, size), APP / f'web/icons/Icon-{size}.png')
        # The complete mark fits within the central 80%-diameter safe circle.
        save_png(render(full_bleed, size), APP / f'web/icons/Icon-maskable-{size}.png')
    render(source, 256).save(APP / 'windows/runner/resources/app_icon.ico',
                            sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    for suffix, color in (('', '#65A30D'), ('-white', '#FFFFFF'), ('-black', '#111827')):
        mark_tree = ET.Element(f'{NS}svg', {'width': '1024', 'height': '1024',
                                          'viewBox': '0 0 1024 1024', 'role': 'img',
                                          'aria-label': 'RustFRP'})
        mark = copy.deepcopy(ET.fromstring(source).find(f'{NS}g'))
        mark.set('fill', color)
        mark_tree.append(mark)
        (BRAND / f'rustfrp-mark{suffix}.svg').write_bytes(ET.tostring(mark_tree) + b'\n')
    print('Regenerated brand, Flutter, Web/PWA, macOS and Windows icons.')


if __name__ == '__main__':
    main()
