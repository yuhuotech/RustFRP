// Compatibility endpoint for the previous panel registration. Contains no renderer or offline cache.
self.addEventListener('install', event => event.waitUntil(self.skipWaiting()));
self.addEventListener('activate', event => event.waitUntil((async () => {
  for (const name of ['flutter-app-cache', 'flutter-temp-cache', 'flutter-app-manifest']) await caches.delete(name);
  await self.clients.claim();
  await self.registration.unregister();
})()));
