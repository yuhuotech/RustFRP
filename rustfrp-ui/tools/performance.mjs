// Actual Chromium renders against the isolated Controller on the fixed development port.
import { chromium } from '@playwright/test';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { setTimeout as pause } from 'node:timers/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const folder = resolve(import.meta.dirname, '../../target/ui-performance');
const base = 'http://127.0.0.1:9674';
const hours = Number(process.env.RUSTFRP_SOAK_HOURS ?? 0);
if (!Number.isFinite(hours) || hours < 0 || hours > 48) throw new Error('Invalid soak duration');
await mkdir(folder, { recursive: true });
const browser = await chromium.launch();
const execute = promisify(execFile);
const cold = [], feedback = [], samples = [], failures = [];
const p95 = values => [...values].sort((a, b) => a - b)[Math.ceil(values.length * 0.95) - 1];
const password = process.env.RUSTFRP_E2E_PASSWORD ?? 'native ui test password';
async function login(page) {
  await page.goto(base, { waitUntil: 'domcontentloaded' });
  await page.getByLabel('账号', { exact: true }).fill('preview');
  await page.getByLabel('密码', { exact: true }).fill(password);
  await page.getByTestId('login-submit').click();
  await page.locator('.metric-card').first().waitFor();
  await page.locator('.traffic-chart, .chart-note').first().waitFor();
}
async function measure(page, client) {
  await client.send('HeapProfiler.collectGarbage');
  const metrics = (await client.send('Performance.getMetrics')).metrics;
  const counters = await client.send('Memory.getDOMCounters');
  const processes = await execute('/bin/ps', ['-axo', 'pid=,rss=,command='], { maxBuffer: 1024 * 1024 });
  const controller = processes.stdout.split('\n').find(line => line.trim().endsWith('/Users/hongmw/data/www/RustFRP/target/release/rustfrp-server run'))?.trim().split(/\s+/, 3);
  return { elapsed_seconds: (Date.now() - started) / 1000, controller_pid: controller?.[0], controller_rss_bytes: controller ? Number(controller[1]) * 1024 : null,
    heap_after_gc: metrics.find(item => item.name === 'JSHeapUsedSize')?.value, ...counters,
    ...await page.evaluate(() => ({ rows: document.querySelectorAll('tbody [data-row-id]').length, dialogs: document.querySelectorAll('dialog').length, scroll_overflow: document.documentElement.scrollWidth > innerWidth, long_tasks: window.__uiPerformance.long, frames: window.__uiPerformance.frames })) };
}
async function feedbackOn(page, selector, event = 'click') {
  return page.evaluate(({ selector, event }) => new Promise(resolve => {
    const target = document.querySelector(selector); if (!target) throw new Error(`Missing ${selector}`);
    const start = performance.now(); target.dispatchEvent(new Event(event, { bubbles: true }));
    requestAnimationFrame(() => requestAnimationFrame(() => resolve(performance.now() - start)));
  }), { selector, event });
}
async function navigate(page, section) {
  if (await page.getByTestId('login-submit').count()) { await page.getByLabel('账号', { exact: true }).fill('preview'); await page.getByLabel('密码', { exact: true }).fill(password); await page.getByTestId('login-submit').click(); await page.locator('.metric-card').first().waitFor(); }
  await page.locator(`.nav-main [data-section="${section}"]`).click();
  if (section === 'tunnels' || section === 'clients') await page.locator('tbody [data-row-id]').first().waitFor();
  else if (section === 'overview') await page.locator('.metric-card').first().waitFor();
  else await page.getByTestId('setting-desktop').or(page.getByTestId('setting-tokens')).first().waitFor();
}
let context;
const started = Date.now();
const result = { version: 1, started_at: new Date(started).toISOString(), browser: browser.version(), platform: process.platform,
  scenario: '50 labelled offline clients / 500 tunnels; 7d traffic fixture; actual Controller API',
  cold_definition: 'fresh browser context; OS and filesystem caches retained', feedback_definition: 'DOM event to second animation frame; excludes network response time',
  gc_definition: 'heap samples after explicit GC; DOM counters and tasks collected independently', requested_soak_hours: hours };
async function save(complete = false) {
  await writeFile(resolve(folder, 'chromium.json'), JSON.stringify({ ...result, complete, actual_elapsed_seconds: (Date.now() - started) / 1000,
    cold, cold_login_p95_ms: cold.length ? p95(cold.map(item => item.login_ready_ms)) : null,
    feedback_ms: feedback, feedback_p95_ms: feedback.length ? p95(feedback) : null, samples, failures }, null, 2) + '\n');
}
try {
  for (let iteration = 0; iteration < 10; iteration++) {
    const fresh = await browser.newContext({ baseURL: base, viewport: { width: 1440, height: 900 } });
    const page = await fresh.newPage(); await page.goto(base, { waitUntil: 'domcontentloaded' });
    await page.getByTestId('login-submit').waitFor(); await page.evaluate(() => document.fonts.ready);
    cold.push(await page.evaluate(() => ({ login_ready_ms: performance.now(), transfer_bytes: performance.getEntriesByType('resource').reduce((sum, entry) => sum + entry.transferSize, 0),
      resources: performance.getEntriesByType('resource').map(entry => ({ name: new URL(entry.name).pathname, transfer_bytes: entry.transferSize })) })));
    await fresh.close();
  }
  context = await browser.newContext({ baseURL: base, viewport: { width: 1440, height: 900 } });
  await context.addInitScript(() => {
    window.__uiPerformance = { long: [], frames: 0 };
    new PerformanceObserver(list => { for (const entry of list.getEntries()) { const values = window.__uiPerformance.long; values.push({ start: entry.startTime, duration: entry.duration }); if (values.length > 128) values.shift(); } }).observe({ type: 'longtask', buffered: true });
    let last = 0; const frame = now => { if (last && now - last > 50) window.__uiPerformance.frames++; last = now; requestAnimationFrame(frame); }; requestAnimationFrame(frame);
  });
  const page = await context.newPage(); page.on('pageerror', error => failures.push(error.message)); await login(page);
  const clients = await (await context.request.get('/api/v1/clients')).json(); const tunnels = await (await context.request.get('/api/v1/tunnels')).json();
  if (clients.filter(item => item.id.startsWith('ui-perf-client-')).length !== 50 || tunnels.filter(item => item.id.startsWith('ui-perf-tunnel-')).length !== 500) throw new Error('The 50/500 performance fixture is missing');
  const cdp = await context.newCDPSession(page); await cdp.send('Performance.enable');
  for (let cycle = 0; cycle < 100; cycle++) await navigate(page, ['clients', 'tunnels', 'overview'][cycle % 3]);
  await navigate(page, 'tunnels');
  if (await page.locator('tbody [data-row-id]').count() > 32) throw new Error('Large table failed to bound rendered rows');
  for (let cycle = 0; cycle < 30; cycle++) {
    feedback.push(await feedbackOn(page, '.collapse'));
    await page.locator('.table-scroll').evaluate(element => { element.scrollTop = element.scrollHeight; });
    await pause(25); if (!(await page.locator('tbody [data-row-id]').last().getAttribute('data-row-id'))?.startsWith('ui-perf-tunnel-')) throw new Error('Virtual table lost final row');
    await page.locator('.table-scroll').evaluate(element => { element.scrollTop = 0; }); await pause(25);
  }
  await navigate(page, 'overview'); await page.locator('[data-range="168"]').click(); await page.locator('[data-range="168"][aria-busy="false"]').waitFor();
  for (const color of ['light', 'dark']) { await page.evaluate(color => { document.documentElement.dataset.theme = color; }, color); await page.screenshot({ path: resolve(folder, `overview-1440-${color}.png`), fullPage: true, animations: 'disabled' }); }
  await navigate(page, 'tunnels'); samples.push(await measure(page, cdp)); await save();
  const until = Date.now() + hours * 3600000; let cycle = 0;
  while (Date.now() < until) {
    await navigate(page, ['clients', 'tunnels', 'overview'][cycle++ % 3]);
    await page.getByTestId('refresh-page').click();
    await pause(Math.min(60000, Math.max(0, until - Date.now())));
    samples.push(await measure(page, cdp)); await save();
  }
  if (failures.length) throw new Error(failures.join('; '));
  if (p95(feedback) > 50) throw new Error(`Feedback p95 exceeded 50ms: ${p95(feedback)}`);
  await save(true);
  console.log(JSON.stringify({ complete: true, cold_login_p95_ms: p95(cold.map(item => item.login_ready_ms)), feedback_p95_ms: p95(feedback), samples: samples.length, actual_elapsed_seconds: (Date.now() - started) / 1000 }, null, 2));
} catch (error) { failures.push(String(error)); await save(false); throw error; }
finally { await context?.close(); await browser.close(); }
