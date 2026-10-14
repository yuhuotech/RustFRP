import { build } from 'esbuild';
import { mkdir, readFile, writeFile, rename, lstat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { gzipSync, brotliCompressSync } from 'node:zlib';
import { resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const dist = resolve(root, 'dist');
await mkdir(dist, { recursive: true });
const history = await readFile(resolve(dist, 'ui-manifest.json'), 'utf8').then(text => {
  if (text.length > 1024 * 1024) throw new Error('UI manifest exceeds bounds');
  const value = JSON.parse(text); if (!Array.isArray(value.entries) || value.entries.length > 256) throw new Error('Invalid previous UI manifest'); return value.entries;
}, error => { if (error.code === 'ENOENT') return []; throw error; });
const result = await build({
  absWorkingDir: root, entryPoints: ['src/main.ts', 'src/styles/app.css'],
  outdir: 'dist', entryNames: '[name]-[hash]', chunkNames: 'chunks/[name]-[hash]',
  bundle: true, splitting: true, format: 'esm', target: ['es2022', 'safari15'],
  minify: true, metafile: true, legalComments: 'eof', charset: 'utf8', write: false,
});
const outputs = Object.entries(result.metafile.outputs);
const js = outputs.find(([, info]) => info.entryPoint === 'src/main.ts')?.[0]?.replace(/^dist\//, '');
const css = outputs.find(([, info]) => info.entryPoint === 'src/styles/app.css')?.[0]?.replace(/^dist\//, '');
if (!js || !css) throw new Error('Missing UI entry output');
const page = (await readFile(resolve(root, 'src/index.html'), 'utf8')).replace('{{SCRIPT}}', js).replace('{{STYLE}}', css);
const entries = [];
const generated = new Map(result.outputFiles.map(file => [file.path.slice(dist.length + 1).replaceAll('\\', '/'), Buffer.from(file.contents)]));
generated.set('index.html', Buffer.from(page));
generated.set('logo.svg', await readFile(resolve(root, '../assets/brand/rustfrp-logo.svg')));
generated.set('flutter_service_worker.js', await readFile(resolve(root, 'src/retire-service-worker.js')));
const retained = new Map();
for (const entry of history) {
  if (generated.has(entry.path) || !/^(?:chunks\/)?[a-zA-Z0-9_-]+-[A-Z0-9]{8}\.(?:js|css)$/.test(entry.path)) continue;
  const until = entry.retained_until ?? Date.now() + 7 * 24 * 3600000;
  if (!Number.isSafeInteger(until) || until < Date.now()) continue;
  const path = resolve(dist, entry.path); const metadata = await lstat(path);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > 16 * 1024 * 1024) throw new Error('Invalid retained UI asset');
  const data = await readFile(path);
  if (createHash('sha256').update(data).digest('hex') !== entry.sha256) throw new Error('Retained UI asset digest mismatch');
  generated.set(entry.path, data); retained.set(entry.path, until);
}
if (generated.size > 255 || [...generated.values()].reduce((sum, data) => sum + data.length, 0) > 16 * 1024 * 1024) throw new Error('UI assets exceed build bounds');
async function atomic(path, data) { await mkdir(resolve(path, '..'), { recursive: true }); const temporary = `${path}.${process.pid}.tmp`; await writeFile(temporary, data, { flag: 'wx' }); await rename(temporary, path); }
for (const [name, data] of generated) {
    const path = resolve(dist, name);
    const gzip = gzipSync(data, { level: 9 });
    const brotli = brotliCompressSync(data);
    if (name !== 'index.html') await atomic(path, data);
    await atomic(`${path}.gz`, gzip);
    await atomic(`${path}.br`, brotli);
    entries.push({ path: name, bytes: data.length, gzip_bytes: gzip.length, brotli_bytes: brotli.length, sha256: createHash('sha256').update(data).digest('hex'), gzip_sha256: createHash('sha256').update(gzip).digest('hex'), brotli_sha256: createHash('sha256').update(brotli).digest('hex'), ...(retained.has(name) ? { retained_until: retained.get(name) } : {}) });
}
const version = JSON.parse(await readFile(resolve(root, 'package.json'), 'utf8')).version;
await atomic(resolve(dist, 'ui-manifest.json'), JSON.stringify({ version, api: 'v1', entries }, null, 2) + '\n');
await atomic(resolve(dist, 'index.html'), page);
await mkdir(resolve(root, '../target/ui'), { recursive: true });
await writeFile(resolve(root, '../target/ui/metafile.json'), JSON.stringify(result.metafile, null, 2));
console.log(`UI ${version}: ${entries.length} assets; ${entries.reduce((n, item) => n + item.gzip_bytes, 0)} gzip bytes`);
