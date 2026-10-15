import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { buildUi } from './build.mjs';

const digest = value => createHash('sha256').update(value).digest('hex');
const hasEntry = (entries, name) => entries.some(entry => entry.path === name);

const root = await mkdtemp(join(tmpdir(), 'rustfrp-ui-build-'));
const source = join(root, 'src');
const dist = join(root, 'dist');
const target = join(root, 'target');
const logo = join(root, 'logo.svg');
const worker = join(source, 'retire-service-worker.js');
try {
  await mkdir(join(source, 'styles'), { recursive: true });
  await writeFile(join(root, 'package.json'), JSON.stringify({ version: 'test' }));
  await writeFile(join(source, 'main.ts'), 'document.body.dataset.ready = "yes";');
  await writeFile(join(source, 'styles/app.css'), 'body { color: #123456; }');
  await writeFile(join(source, 'index.html'), '<!doctype html><html><head><meta name="rustfrp-ui-version" content="{{VERSION}}"><link rel="stylesheet" href="{{STYLE}}"></head><body><script type="module" src="{{SCRIPT}}"></script></body></html>');
  await writeFile(worker, 'self.addEventListener("install", () => self.skipWaiting());');
  await writeFile(logo, '<svg xmlns="http://www.w3.org/2000/svg"></svg>');

  const first = await buildUi({ root, dist, targetDir: target, logoPath: logo, workerPath: worker });
  const currentIndex = await readFile(join(dist, 'index.html'), 'utf8');
  const oldName = 'chunks/old-ABCDEF12.js';
  const oldBody = 'old-version';
  await mkdir(join(dist, 'chunks'), { recursive: true });
  await writeFile(join(dist, oldName), oldBody);
  await writeFile(join(dist, 'ui-manifest.json'), JSON.stringify({
    version: 'test',
    entries: [{ path: oldName, sha256: digest(oldBody), retained_until: Date.now() + 60_000 }],
  }));

  const second = await buildUi({ root, dist, targetDir: target, logoPath: logo, workerPath: worker });
  const manifest = JSON.parse(await readFile(join(dist, 'ui-manifest.json'), 'utf8'));
  assert.equal(hasEntry(first.entries, 'index.html'), true);
  assert.equal(second.entries.some(entry => entry.path === oldName && entry.retained_until), true);
  assert.equal(manifest.entries.some(entry => entry.path === oldName), true);
  assert.equal(await readFile(join(dist, oldName), 'utf8'), oldBody);

  await writeFile(join(dist, oldName), 'tampered');
  const beforeFailure = await readFile(join(dist, 'index.html'), 'utf8');
  await assert.rejects(
    buildUi({ root, dist, targetDir: target, logoPath: logo, workerPath: worker }),
    /Retained UI asset digest mismatch/,
  );
  assert.equal(await readFile(join(dist, 'index.html'), 'utf8'), beforeFailure);

  await writeFile(join(dist, oldName), oldBody);
  await writeFile(join(dist, `${oldName}.gz`), 'old-gzip');
  await writeFile(join(dist, `${oldName}.br`), 'old-br');
  await writeFile(join(dist, 'ui-manifest.json'), JSON.stringify({
    version: 'test',
    entries: [{ path: oldName, sha256: digest(oldBody), retained_until: Date.now() - 1 }],
  }));
  const third = await buildUi({ root, dist, targetDir: target, logoPath: logo, workerPath: worker });
  const finalManifest = JSON.parse(await readFile(join(dist, 'ui-manifest.json'), 'utf8'));
  assert.equal(hasEntry(third.entries, oldName), false);
  assert.equal(finalManifest.entries.some(entry => entry.path === oldName), false);
  await assert.rejects(access(join(dist, oldName)));
  await assert.rejects(access(join(dist, `${oldName}.gz`)));
  await assert.rejects(access(join(dist, `${oldName}.br`)));
  assert.equal(await readFile(join(dist, 'index.html'), 'utf8'), currentIndex);
  const custom = join(root, 'custom-output');
  const relocated = await buildUi({ root, dist: custom, targetDir: target, logoPath: logo, workerPath: worker });
  assert.equal(hasEntry(relocated.entries, 'index.html'), true);
  assert.equal(await readFile(join(custom, 'index.html'), 'utf8'), currentIndex);
  assert.equal(relocated.entries.every(entry => !entry.path.includes('..') && !entry.path.startsWith('/')), true);
  await assert.rejects(buildUi({ root, dist, targetDir: target, logoPath: logo, workerPath: worker, version: '<script>' }), /Invalid UI version/);
  await buildUi({ root, dist: custom, targetDir: target, logoPath: logo, workerPath: worker, version: '1.2.3' });
  assert.match(await readFile(join(custom, 'index.html'), 'utf8'), /content="1.2.3"/);
  console.log('build.test.mjs: retained, tampered, expired, and atomic publish checks passed');
} finally {
  await rm(root, { recursive: true, force: true });
}
