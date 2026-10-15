import { build } from 'esbuild';
import { mkdir, readFile, writeFile, rename, lstat, unlink } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { gzipSync, brotliCompressSync } from 'node:zlib';
import { relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const defaultRoot = resolve(import.meta.dirname, '..');
const hashedAsset = /^(?:chunks\/)?[a-zA-Z0-9_-]+-[A-Z0-9]{8}\.(?:js|css)$/;

async function removeExpiredAsset(dist, name) {
  for (const suffix of ['', '.gz', '.br']) {
    const path = resolve(dist, `${name}${suffix}`);
    try {
      const metadata = await lstat(path);
      if (metadata.isFile() && !metadata.isSymbolicLink()) await unlink(path);
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }
}

export async function buildUi(options = {}) {
  const root = resolve(options.root ?? defaultRoot);
  const dist = resolve(options.dist ?? resolve(root, 'dist'));
  const logoPath = resolve(options.logoPath ?? resolve(root, '../assets/brand/rustfrp-logo.svg'));
  const workerPath = resolve(options.workerPath ?? resolve(root, 'src/retire-service-worker.js'));
  const targetDir = resolve(options.targetDir ?? resolve(root, '../target/ui'));
  const packageVersion = JSON.parse(await readFile(resolve(root, 'package.json'), 'utf8')).version;
  const version = options.version ?? process.env.RUSTFRP_RELEASE_VERSION?.replace(/^v/, '') ?? packageVersion;
  if (typeof version !== 'string' || !/^[a-zA-Z0-9][a-zA-Z0-9.+_-]{0,63}$/.test(version)) throw new Error('Invalid UI version');
  await mkdir(dist, { recursive: true });
  const history = await readFile(resolve(dist, 'ui-manifest.json'), 'utf8').then(text => {
    if (text.length > 1024 * 1024) throw new Error('UI manifest exceeds bounds');
    const value = JSON.parse(text);
    if (!Array.isArray(value.entries) || value.entries.length > 256) throw new Error('Invalid previous UI manifest');
    return value.entries;
  }, error => { if (error.code === 'ENOENT') return []; throw error; });
  const result = await build({
    absWorkingDir: root, entryPoints: ['src/main.ts', 'src/styles/app.css'],
    outdir: dist, entryNames: '[name]-[hash]', chunkNames: 'chunks/[name]-[hash]',
    bundle: true, splitting: true, format: 'esm', target: ['es2022', 'safari15'],
    minify: true, metafile: true, legalComments: 'eof', charset: 'utf8', write: false,
  });
  const outputs = Object.entries(result.metafile.outputs);
  const outputName = name => name && relative(dist, resolve(root, name)).replaceAll('\\', '/');
  const js = outputName(outputs.find(([, info]) => info.entryPoint === 'src/main.ts')?.[0]);
  const css = outputName(outputs.find(([, info]) => info.entryPoint === 'src/styles/app.css')?.[0]);
  if (!js || !css) throw new Error('Missing UI entry output');
  const page = (await readFile(resolve(root, 'src/index.html'), 'utf8')).replace('{{SCRIPT}}', js).replace('{{STYLE}}', css).replace('{{VERSION}}', version);
  const entries = [];
  const generated = new Map(result.outputFiles.map(file => [file.path.slice(dist.length + 1).replaceAll('\\', '/'), Buffer.from(file.contents)]));
  generated.set('index.html', Buffer.from(page));
  generated.set('logo.svg', await readFile(logoPath));
  generated.set('flutter_service_worker.js', await readFile(workerPath));
  const retained = new Map();
  const expired = [];
  for (const entry of history) {
    if (generated.has(entry.path) || !hashedAsset.test(entry.path)) continue;
    const until = entry.retained_until ?? Date.now() + 7 * 24 * 3600000;
    if (!Number.isSafeInteger(until) || until < Date.now()) {
      expired.push(entry.path);
      continue;
    }
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
  await atomic(resolve(dist, 'ui-manifest.json'), JSON.stringify({ version, api: 'v1', entries }, null, 2) + '\n');
  await atomic(resolve(dist, 'index.html'), page);
  for (const name of expired) await removeExpiredAsset(dist, name);
  await mkdir(targetDir, { recursive: true });
  await writeFile(resolve(targetDir, 'metafile.json'), JSON.stringify(result.metafile, null, 2));
  return { version, entries, metafile: result.metafile };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = await buildUi();
  console.log(`UI ${result.version}: ${result.entries.length} assets; ${result.entries.reduce((n, item) => n + item.gzip_bytes, 0)} gzip bytes`);
}
