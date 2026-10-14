import { describe, expect, it, vi } from 'vitest';

describe('previous panel cache retirement', () => {
  it('takes over one old registration, removes only the known panel caches and unregisters', async () => {
    const handlers: Record<string, (event: { waitUntil(work: Promise<unknown>): void }) => void> = {};
    const skipWaiting = vi.fn().mockResolvedValue(undefined), claim = vi.fn().mockResolvedValue(undefined), unregister = vi.fn().mockResolvedValue(true), remove = vi.fn().mockResolvedValue(true);
    vi.stubGlobal('self', { addEventListener: (name: string, listener: typeof handlers[string]) => { handlers[name] = listener; }, skipWaiting, clients: { claim }, registration: { unregister } });
    vi.stubGlobal('caches', { delete: remove });
    try {
      await import('../src/retire-service-worker.js');
      let pending: Promise<unknown> | undefined;
      handlers.install({ waitUntil(work) { pending = work; } }); await pending; expect(skipWaiting).toHaveBeenCalledOnce();
      handlers.activate({ waitUntil(work) { pending = work; } }); await pending;
      expect(remove.mock.calls).toEqual([['flutter-app-cache'], ['flutter-temp-cache'], ['flutter-app-manifest']]);
      expect(claim).toHaveBeenCalledOnce(); expect(unregister).toHaveBeenCalledOnce(); expect(claim.mock.invocationCallOrder[0]).toBeLessThan(unregister.mock.invocationCallOrder[0]);
    } finally { vi.unstubAllGlobals(); }
  });
});
