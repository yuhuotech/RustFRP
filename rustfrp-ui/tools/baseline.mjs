import { chromium } from '@playwright/test';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const folder = resolve(import.meta.dirname, '../../target/ui-refactor-baseline');
await mkdir(folder, { recursive: true });
const browser = await chromium.launch();
const samples = [];
try {
  for (let iteration = 0; iteration < 3; iteration++) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    const page = await context.newPage();
    await page.addInitScript(() => {
      window.addEventListener('flutter-first-frame', () => { window.__rustfrpFirstFrame = performance.now(); });
    });
    await page.goto('http://127.0.0.1:9674', { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => window.__rustfrpFirstFrame > 0, { timeout: 30000 });
    const timing = await page.evaluate(() => ({
      first_frame_ms: window.__rustfrpFirstFrame,
      transfer_bytes: performance.getEntriesByType('resource').reduce((sum, entry) => sum + entry.transferSize, 0),
      resources: performance.getEntriesByType('resource').map(entry => ({ name: new URL(entry.name).pathname, bytes: entry.transferSize, duration_ms: entry.duration })),
    }));
    samples.push(timing);
    if (!iteration) await page.screenshot({ path: resolve(folder, 'flutter-login.png') });
    await context.close();
  }
  const result = { commit: '6058ade', browser: browser.version(), viewport: [1440, 900], cache: 'fresh browser context; OS caches not reset', criterion: 'flutter-first-frame; login without a session', samples };
  await writeFile(resolve(folder, 'flutter-browser.json'), JSON.stringify(result, null, 2) + '\n');
  console.log(JSON.stringify({ browser: result.browser, first_frame_ms: samples.map(sample => sample.first_frame_ms), transfer_bytes: samples.map(sample => sample.transfer_bytes) }));
} finally { await browser.close(); }
