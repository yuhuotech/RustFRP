import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const manifest = JSON.parse(await readFile(resolve(import.meta.dirname, '../dist/ui-manifest.json'), 'utf8'));
const metadata = JSON.parse(await readFile(resolve(import.meta.dirname, '../../target/ui/metafile.json'), 'utf8'));
const js = manifest.entries.filter(item => !item.retained_until && item.path.endsWith('.js')).reduce((n, item) => n + item.gzip_bytes, 0);
const css = manifest.entries.filter(item => !item.retained_until && item.path.endsWith('.css')).reduce((n, item) => n + item.gzip_bytes, 0);
const retained = manifest.entries.filter(item => item.retained_until).reduce((n, item) => n + item.gzip_bytes, 0);
const initial = new Set(['index.html', 'logo.svg']);
function follow(path) {
  const name = path.replace(/^dist\//, ''); if (initial.has(name)) return; initial.add(name);
  for (const item of metadata.outputs[path]?.imports ?? []) if (item.kind !== 'dynamic-import' && !item.external) follow(item.path);
}
for (const [path, output] of Object.entries(metadata.outputs)) if (output.entryPoint === 'src/main.ts' || output.entryPoint === 'src/styles/app.css') follow(path);
const first = manifest.entries.filter(item => initial.has(item.path)).reduce((n, item) => n + item.gzip_bytes, 0);
const limits = { js: 120 * 1024, css: 35 * 1024, first: 200 * 1024, all: 500 * 1024 };
const result = { version: manifest.version, initial_assets: [...initial], gzip_bytes: { js, css, first, all: js + css, retained, distribution: js + css + retained }, limits };
console.log(JSON.stringify(result, null, 2));
if (js > limits.js || css > limits.css || first > limits.first || js + css + retained > limits.all) throw new Error('UI resource budget exceeded');
