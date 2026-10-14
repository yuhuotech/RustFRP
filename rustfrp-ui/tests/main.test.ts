import { afterEach, expect, it, vi } from 'vitest';
afterEach(() => { window.dispatchEvent(new Event('pagehide')); vi.unstubAllGlobals(); vi.resetModules(); });
it('the actual entrypoint starts the application and disposes page state when the document leaves', async () => {
  vi.resetModules(); const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'unauthorized', message: '请登录' } }), { status: 401 })); vi.stubGlobal('fetch', fetcher);
  await import('../src/main'); await vi.waitFor(() => expect(document.querySelector('#login-name')).not.toBeNull()); expect(fetcher).toHaveBeenCalledOnce(); window.dispatchEvent(new Event('pagehide')); expect(document.getElementById('app')!.children).toHaveLength(0);
});
it('the entrypoint does nothing when its root is absent', async () => {
  vi.resetModules(); document.getElementById('app')!.remove(); const fetcher = vi.fn(); vi.stubGlobal('fetch', fetcher); await import('../src/main'); expect(fetcher).not.toHaveBeenCalled();
});
