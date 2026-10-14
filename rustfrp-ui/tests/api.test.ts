import { describe, expect, it, vi } from 'vitest';
import { ApiClient, ApiError, requestPath } from '../src/api/client';
import { Resources } from '../src/app/resources';
import { DesktopHost, WebHost, nativeError } from '../src/platform/host';

const response = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } });
it('accepts additive v1 negotiation and legacy sessions while rejecting explicit incompatible APIs', async () => {
  const fetcher = vi.fn().mockResolvedValueOnce(response({ api: 'v2', csrf: 'untrusted' })).mockResolvedValueOnce(response({ api: 'v1', csrf: 'current' })).mockResolvedValueOnce(response({ csrf: 'legacy' }));
  const api = new ApiClient(undefined, fetcher);
  await expect(api.request('me')).rejects.toMatchObject({ code: 'incompatible_api', uncertain: false }); expect(api.csrf).toBe('');
  await api.request('login', { body: {} }); expect(api.csrf).toBe('current'); await api.request('me'); expect(api.csrf).toBe('legacy'); api.close();
});
describe('Controller request contract and session boundary', () => {
  it('encodes resource identifiers and reasons without changing the selected route', () => {
    expect(requestPath('deleteClient', { id: 'a/b ?#', query: { reason: '测试 & 发布' } })).toBe('/api/v1/clients/a%2Fb%20%3F%23?reason=%E6%B5%8B%E8%AF%95+%26+%E5%8F%91%E5%B8%83');
    expect(() => requestPath('client', {})).toThrow('资源标识');
    expect(requestPath('clients', {})).toBe('/api/v1/clients');
  });
  it('uses browser cookies and attaches CSRF only to writes', async () => {
    const fetcher = vi.fn().mockResolvedValueOnce(response({ csrf: 'test-csrf', admin: { id: 'a' } })).mockImplementation(() => Promise.resolve(response({ ok: true })));
    const api = new ApiClient(undefined, fetcher);
    await api.request('login', { body: { name: 'root', password: 'test' } });
    expect(fetcher.mock.calls[0][1].headers).toMatchObject({ 'x-rustfrp-admin': '1' });
    await api.request('patchClient', { id: 'id', body: { reason: '维护' } });
    expect(fetcher.mock.calls[1][1]).toMatchObject({ credentials: 'same-origin', method: 'PATCH', headers: { 'x-csrf-token': 'test-csrf' } });
    await api.request('clients'); expect(fetcher.mock.calls[2][1].headers).not.toHaveProperty('x-csrf-token');
    await api.request('logout', { body: {} }); expect(api.csrf).toBe('');
  });
  it('keeps wrong-password errors separate from session expiry and preserves backend errors', async () => {
    const fetcher = vi.fn().mockImplementation(() => Promise.resolve(response({ error: { code: 'unauthorized', message: '登录失败' } }, 401)));
    const api = new ApiClient(undefined, fetcher); api.csrf = 'session'; api.onUnauthorized = vi.fn();
    await expect(api.request('login', { body: {} })).rejects.toMatchObject({ status: 401, message: '登录失败' });
    expect(api.onUnauthorized).not.toHaveBeenCalled();
    await expect(api.request('me')).rejects.toMatchObject({ code: 'unauthorized' });
    expect(api.onUnauthorized).toHaveBeenCalledOnce(); expect(api.csrf).toBe('');
  });
  it('surfaces HTML proxy errors and marks a failed write as uncertain', async () => {
    const api = new ApiClient(undefined, vi.fn().mockResolvedValue(new Response('<html>Error</html>', { status: 502 })));
    await expect(api.request('createTunnel', { body: { reason: '发布' } })).rejects.toMatchObject({ status: 502, code: 'invalid_response', uncertain: true });
    const broken = new ApiClient(undefined, vi.fn().mockResolvedValue(new Response('not json')));
    await expect(broken.request('clients')).rejects.toMatchObject({ code: 'invalid_response', uncertain: false });
  });
  it('cancels reads, bounds native request time, and never automatically retries writes', async () => {
    const failure = vi.fn().mockRejectedValue(new Error('offline')); const api = new ApiClient(undefined, failure);
    await expect(api.request('createTunnel', { body: {} })).rejects.toMatchObject({ code: 'network', uncertain: true }); expect(failure).toHaveBeenCalledOnce();
    const controller = new AbortController(); controller.abort();
    await expect(api.request('clients', {}, controller.signal)).rejects.toMatchObject({ code: 'cancelled' });
    const native = { request: vi.fn(() => new Promise<never>(() => {})) };
    const timed = new ApiClient(native, failure, 5);
    await expect(timed.request('clients')).rejects.toMatchObject({ code: 'timeout' });
    timed.close();
  });
  it('accepts empty successful bodies and unknown HTTP error shapes', async () => {
    const api = new ApiClient(undefined, vi.fn().mockResolvedValueOnce(new Response(null, { status: 204 })).mockResolvedValueOnce(response([], 403)));
    expect(await api.request('logout')).toBeNull();
    await expect(api.request('clients')).rejects.toMatchObject({ status: 403, code: 'http_403' });
  });
});

describe('bounded query state', () => {
  it('deduplicates reads, keeps a fresh cache and rejects stale responses after a write', async () => {
    let resolve!: (value: unknown) => void;
    const fetcher = vi.fn(() => new Promise<Response>(done => { resolve = value => done(response(value)); }));
    const api = new ApiClient(undefined, fetcher); const resources = new Resources(api); const changed = vi.fn(); const unsub = resources.subscribe(changed);
    const first = resources.read('clients', 'clients'); const duplicate = resources.read('clients', 'clients');
    expect(fetcher).toHaveBeenCalledOnce(); resolve([{ id: 'a' }]);
    expect(await first).toEqual(await duplicate); expect(changed).toHaveBeenCalledOnce();
    await resources.read('clients', 'clients'); expect(fetcher).toHaveBeenCalledOnce();
    const stale = resources.read('clients', 'clients', {}, 0); resources.invalidate(); resolve([{ id: 'old' }]); await stale;
    expect(resources.get('clients')).toEqual([{ id: 'a' }]); unsub(); resources.dispose(); api.close();
  });
  it('evicts old entries and does not redraw unchanged data', async () => {
    const api = new ApiClient(undefined, vi.fn().mockImplementation(() => Promise.resolve(response([])))); const resources = new Resources(api, 1, () => 100); const changed = vi.fn(); resources.subscribe(changed);
    await resources.read('a', 'clients'); await resources.read('a', 'clients', {}, 0); expect(changed).toHaveBeenCalledOnce();
    await resources.read('b', 'nodes'); expect(resources.get('a')).toBeUndefined(); expect(resources.get('b')).toEqual([]);
    resources.cancel(); resources.clear(); resources.dispose(); api.close();
  });
});

describe('desktop and browser adapters', () => {
  it('normalizes native unknown failures without losing uncertain write status', async () => {
    const original = new Error('original'); expect(nativeError(original)).toBe(original); expect(nativeError({ status: 0, message: 'unknown write', uncertain: true })).toMatchObject({ code: 'native', uncertain: true }); expect(nativeError(null).message).toContain('桌面操作失败');
    const dispatch = vi.fn().mockRejectedValue('cancel rejected'); const host = new DesktopHost(dispatch); const abort = new AbortController(); abort.abort(); await expect(host.request('clients', {}, abort.signal)).rejects.toThrow('aborted');
    const clipboard = vi.fn().mockResolvedValue(undefined); Object.defineProperty(navigator, 'clipboard', { value: { writeText: clipboard }, configurable: true }); await new WebHost().call('copy'); expect(clipboard).toHaveBeenCalledWith('');
  });
  it('uses typed IPC operations and normalizes native auth failures', async () => {
    const dispatch = vi.fn().mockResolvedValue({ desktop: true }); const host = new DesktopHost(dispatch);
    await host.call('info'); expect(dispatch).toHaveBeenCalledWith('host_command', { action: 'info', args: {} });
    await host.request('clients', {}); expect(dispatch).toHaveBeenCalledWith('controller_request', expect.objectContaining({ operation: 'clients', args: {} }));
    dispatch.mockRejectedValue({ status: 401, code: 'unauthorized', message: 'expired' });
    await expect(host.request('me', {})).rejects.toBeInstanceOf(ApiError);
    dispatch.mockRejectedValue('offline'); await expect(host.request('clients', {})).rejects.toThrow('offline');
    dispatch.mockRejectedValue({ status: 0, code: 'local_io', message: '配置不可读' }); await expect(host.call('readConfig')).rejects.toThrow('配置不可读');
  });
  it('requests native cancellation and refuses to start an already cancelled read', async () => {
    let resolve!: (value: null) => void;
    const dispatch = vi.fn().mockImplementation((name: string) => name === 'controller_request' ? new Promise<null>(done => { resolve = done; }) : Promise.resolve(null));
    const host = new DesktopHost(dispatch); const abort = new AbortController(); const request = host.request('clients', {}, abort.signal); abort.abort();
    expect(dispatch).toHaveBeenCalledWith('controller_cancel', expect.objectContaining({ requestId: expect.any(String) })); resolve(null); await request;
    await expect(host.request('clients', {}, abort.signal)).rejects.toThrow('aborted');
  });
  it('limits the browser host to safe links and clipboard operations', async () => {
    const clipboard = vi.fn().mockResolvedValue(undefined); Object.defineProperty(navigator, 'clipboard', { value: { writeText: clipboard }, configurable: true });
    const open = vi.spyOn(window, 'open').mockImplementation(() => null); const host = new WebHost();
    await host.call('copy', { text: 'hello' }); expect(clipboard).toHaveBeenCalledWith('hello');
    await host.call('openUrl', { url: 'https://example.com' }); expect(open).toHaveBeenCalledWith('https://example.com/', '_blank', 'noopener,noreferrer');
    await expect(host.call('openUrl', { url: 'javascript:alert(1)' })).rejects.toThrow('不支持');
    await expect(host.call('localStart')).rejects.toThrow('桌面客户端');
    await expect(host.request()).rejects.toThrow('同源 HTTP'); expect(await host.call('info')).toMatchObject({ desktop: false });
  });
});
