import { test, expect } from '@playwright/test';
import { createServer, get } from 'node:http';

test('retires the legacy Flutter worker while preserving unrelated browser caches', async ({ page }) => {
  await page.goto('/');
  const result = await page.evaluate(async () => {
    const known = ['flutter-app-cache', 'flutter-temp-cache', 'flutter-app-manifest'];
    const unrelated = 'rustfrp-unrelated-cache';
    await Promise.all([...known, unrelated].map(name => caches.open(name)));
    try {
      const registration = await navigator.serviceWorker.register('/flutter_service_worker.js', { scope: '/' });
      const worker = registration.installing ?? registration.waiting ?? registration.active;
      if (!worker) throw new Error('legacy worker did not install');
      const deadline = performance.now() + 10000;
      // Firefox may leave the detached descriptor at "activating" after unregister.
      // Assert the visible cache and registration outcome rather than this descriptor.
      while ((await navigator.serviceWorker.getRegistrations()).length || (await caches.keys()).some(name => known.includes(name))) {
        if (performance.now() >= deadline) throw new Error(`retirement did not complete (${worker.state})`);
        await new Promise(resolve => setTimeout(resolve, 50));
      }
      return {
        caches: await caches.keys(),
        registrations: (await navigator.serviceWorker.getRegistrations()).length,
      };
    } finally {
      await Promise.all([...known, unrelated].map(name => caches.delete(name)));
      await Promise.all((await navigator.serviceWorker.getRegistrations()).map(item => item.unregister()));
    }
  });
  expect(result.caches).not.toContain('flutter-app-cache');
  expect(result.caches).not.toContain('flutter-temp-cache');
  expect(result.caches).not.toContain('flutter-app-manifest');
  expect(result.caches).toContain('rustfrp-unrelated-cache');
  expect(result.registrations).toBe(0);
});

test('upgrades a page actually controlled by the old cached panel to the native entry', async ({ page }) => {
  const legacy = `
    self.addEventListener('install', event => event.waitUntil((async () => {
      const cache = await caches.open('flutter-app-cache');
      await cache.put('/', new Response('<!doctype html><html><body><p data-old-panel>cached old panel</p></body></html>', { headers: { 'Content-Type': 'text/html' } }));
      await caches.open('flutter-temp-cache'); await caches.open('flutter-app-manifest');
      await self.skipWaiting();
    })()));
    self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
    self.addEventListener('fetch', event => {
      if (new URL(event.request.url).pathname === '/') event.respondWith(caches.match('/').then(cached => cached || fetch(event.request)));
    });`;
  let legacyActive = true;
  // Real HTTP is required: Firefox does not consistently route service-worker
  // script requests through Playwright interception. All other reads use the
  // current Controller's assets; the fixture never forwards a write.
  const server = createServer((request, response) => {
    if (!['GET', 'HEAD'].includes(request.method ?? '')) { response.writeHead(405).end(); return; }
    if (request.url === '/flutter_service_worker.js' && legacyActive) {
      response.writeHead(200, { 'Content-Type': 'application/javascript', 'Cache-Control': 'no-store' }).end(legacy); return;
    }
    const upstream = get(`http://127.0.0.1:9674${request.url}`, { headers: { ...request.headers, host: '127.0.0.1:9674', origin: 'http://127.0.0.1:9674' } }, incoming => {
      response.writeHead(incoming.statusCode ?? 502, incoming.headers); incoming.pipe(response);
    });
    upstream.on('error', () => { if (!response.headersSent) response.writeHead(502); response.end(); });
  });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address(); if (!address || typeof address === 'string') throw new Error('missing worker fixture address');
  try {
    await page.goto(`http://127.0.0.1:${address.port}/`);
    await page.evaluate(async () => {
      await navigator.serviceWorker.register('/flutter_service_worker.js', { scope: '/', updateViaCache: 'none' });
      await navigator.serviceWorker.ready;
      if (!navigator.serviceWorker.controller) await new Promise<void>(resolve => navigator.serviceWorker.addEventListener('controllerchange', () => resolve(), { once: true }));
      await caches.open('unrelated-upgrade-cache');
    });
    await page.reload();
    await expect(page.locator('[data-old-panel]')).toHaveText('cached old panel');
    legacyActive = false;
    await page.evaluate(async () => {
      const registration = await navigator.serviceWorker.getRegistration('/');
      if (!registration) throw new Error('old controller is missing');
      // The retiring worker unregisters itself during activation. Firefox can
      // reject this promise after that succeeds, so the assertions below use
      // the actual registration/cache/page outcome as the upgrade result.
      void registration.update().catch(() => undefined);
    });
    await expect.poll(() => page.evaluate(async () => ({
      registrations: (await navigator.serviceWorker.getRegistrations()).length,
      legacy: (await caches.keys()).filter(name => name.startsWith('flutter-')).length,
    }))).toEqual({ registrations: 0, legacy: 0 });
    expect(await page.evaluate(() => caches.has('unrelated-upgrade-cache'))).toBe(true);
    await page.reload();
    await expect(page.getByTestId('login-submit')).toBeVisible();
    expect(await page.locator('[data-old-panel]').count()).toBe(0);
  } finally {
    await page.evaluate(async () => {
      await Promise.all((await navigator.serviceWorker.getRegistrations()).map(registration => registration.unregister()));
      await Promise.all((await caches.keys()).map(name => caches.delete(name)));
    });
    server.closeAllConnections();
    await new Promise<void>(resolve => server.close(() => resolve()));
  }
});
