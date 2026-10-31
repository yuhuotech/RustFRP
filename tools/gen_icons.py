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
    (ROOT / 'rustfrp-web/logo.svg').write_bytes(source)
    for size in (16, 32, 64, 128, 256, 512, 1024):
        image = render(source, size)
        save_png(image, BRAND / f'rustfrp-logo-{size}.png')
    render(source, 1024).save(BRAND / 'rustfrp-logo.icns')
    render(source, 256).save(BRAND / 'rustfrp-logo.ico',
                            sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    for suffix, color in (('', '#65A30D'), ('-white', '#FFFFFF'), ('-black', '#111827')):
        mark_tree = ET.Element(f'{NS}svg', {'width': '1024', 'height': '1024',
                                          'viewBox': '0 0 1024 1024', 'role': 'img',
                                          'aria-label': 'RustFRP'})
        mark = copy.deepcopy(ET.fromstring(source).find(f'{NS}g'))
        mark.set('fill', color)
        mark_tree.append(mark)
        (BRAND / f'rustfrp-mark{suffix}.svg').write_bytes(ET.tostring(mark_tree) + b'\n')
    print('Regenerated brand, website, macOS ICNS and Windows ICO icons.')


if __name__ == '__main__':
    main()
