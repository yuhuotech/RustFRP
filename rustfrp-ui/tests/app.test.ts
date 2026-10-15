import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../src/app/app';
import { ApiClient } from '../src/api/client';
import { WebHost, type Host } from '../src/platform/host';
import type { Json } from '../src/api/models';

const apps: App[] = [];
afterEach(() => { for (const app of apps.splice(0)) app.dispose(); vi.useRealTimers(); });
const client = { id: 'c', name: '中文 NAS', online: true, tags: ['生产'], node_id: '', os: 'linux', arch: 'x86_64', version: '0.1.0', last_ip: '203.0.113.1', last_seen_at: 1700000000000, disabled_at: null };
const node = { id: '', name: '内置节点', builtin: true, online: true, clients: 1, public_host: 'example.com', edge_port: 7000, version: '0.1.0', vhost_http: 80, disabled_at: null };
function harness(currentRole = 'owner', authenticated = true, host: Host = new WebHost()) {
  let signedIn = authenticated;
  const requests: { path: string; method: string }[] = [];
  const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input).split('?')[0]; const method = init?.method ?? 'GET'; requests.push({ path, method });
    if (path.endsWith('/auth/login')) { signedIn = true; return new Response(JSON.stringify({ csrf: 'test', admin: { id: 'a', name: 'root', role: currentRole, disabled_at: null } })); }
    if (path.endsWith('/auth/logout')) { signedIn = false; return new Response('{}'); }
    if (!signedIn) return new Response(JSON.stringify({ error: { code: 'unauthorized', message: '登录失败' } }), { status: 401 });
    const data: Record<string, Json> = {
      '/api/v1/auth/me': { csrf: 'test', admin: { id: 'a', name: 'root', role: currentRole, disabled_at: null } },
      '/api/v1/overview': { counts: { tunnels_running: 0, tokens_pending: 0 }, clients_online: 1, traffic: { connections: 0 }, traffic_24h: { bytes_in: 0, bytes_out: 0 }, process: { rss_bytes: null }, version: '0.1.0' },
      '/api/v1/nodes': [node], '/api/v1/clients': [client], '/api/v1/tunnels': [],
      '/api/v1/traffic': { range_hours: 24, window_end: 1700000000000, bucket_ms: 3600000, items: [] },
    };
    return new Response(JSON.stringify(data[path] ?? {}));
  });
  const app = new App(document.getElementById('app')!, host, new ApiClient(undefined, fetcher)); apps.push(app);
  return { app, requests, fetcher };
}
describe('shared application lifecycle', () => {
  it('polling pauses for hidden pages and open dialogs and session expiry disposes resource state', async () => {
    vi.useFakeTimers(); const { app, fetcher } = harness(); await app.start(); await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6)); const before = fetcher.mock.calls.length;
    Object.defineProperty(document, 'hidden', { value: true, configurable: true }); document.dispatchEvent(new Event('visibilitychange')); await vi.advanceTimersByTimeAsync(5000); expect(fetcher.mock.calls.length).toBe(before);
    Object.defineProperty(document, 'hidden', { value: false, configurable: true }); const dialog = document.createElement('dialog'); dialog.setAttribute('open', ''); document.body.append(dialog); await vi.advanceTimersByTimeAsync(5000); expect(fetcher.mock.calls.length).toBe(before); dialog.remove(); document.dispatchEvent(new Event('visibilitychange')); await vi.advanceTimersByTimeAsync(0); expect(fetcher.mock.calls.length).toBeGreaterThan(before);
    app.api.onUnauthorized(); expect(document.querySelector('#login-password')).not.toBeNull(); expect(app.resources.get('clients')).toBeUndefined(); vi.useRealTimers();
  });
  it('native preference failures and logout network failures remain visible and clear local admin state', async () => {
    const host: Host = { desktop: true, call: vi.fn().mockRejectedValue(new Error('private preferences unavailable')), request: vi.fn() }; const first = harness('owner', true, host); await first.app.start(); expect(document.querySelector('.form-error')?.textContent).toBe('private preferences unavailable'); first.app.dispose();
    const second = harness(); await second.app.start(); second.app.navigate('settings'); await vi.waitFor(() => expect(document.body.textContent).toContain('退出登录')); second.fetcher.mockRejectedValue(new Error('offline')); [...document.querySelectorAll('button')].find(control => control.textContent === '退出登录')!.click(); await vi.waitFor(() => expect(document.querySelector('#login-name')).not.toBeNull()); expect(document.querySelector('.toast.danger')).not.toBeNull();
  });
  it('loads the last native page and preferences without exposing native sessions to the DOM', async () => {
    const host: Host = { desktop: true, call: vi.fn(async action => action === 'preferences' ? { theme: 'dark', sidebar_collapsed: true, controller_url: 'https://controller.example.com' } : null), request: vi.fn() };
    localStorage.setItem('rustfrp.page', JSON.stringify({ section: 'clients', id: 'c' }));
    const { app } = harness('owner', true, host); await app.start(); await vi.waitFor(() => expect(document.body.textContent).toContain('中文 NAS'));
    expect(document.documentElement.dataset.theme).toBe('dark'); expect(document.documentElement.dataset.collapsed).toBe('true'); expect(host.call).toHaveBeenCalledWith('selectController', { url: 'https://controller.example.com' });
    expect(document.querySelector('.collapse svg')).not.toBeNull(); expect(document.querySelector('.collapse')!.getAttribute('aria-label')).toBe('展开菜单');
    (document.querySelector('.collapse') as HTMLButtonElement).click(); expect(host.call).toHaveBeenCalledWith('savePreferences', { sidebar_collapsed: false });
    expect(document.querySelector('.collapse')!.getAttribute('aria-label')).toBe('收起菜单'); expect(document.querySelector('.collapse')!.getAttribute('aria-expanded')).toBe('true');
    app.navigate('tunnels'); app.navigate('nodes'); app.navigate('settings', 'appearance'); await vi.waitFor(() => expect(document.body.textContent).toContain('跟随系统')); expect(document.querySelector('[data-testid=page-title]')?.textContent).toBe('系统设置'); expect(host.call).toHaveBeenCalledWith('savePreferences', { local_mode: false });
  });
  it('starts native local mode without a controller session and keeps local access separate from admin navigation', async () => {
    const host: Host = { desktop: true, call: vi.fn(async action => action === 'preferences' ? { local_mode: true, local: { configFile: '/test/frpc.toml' } } : null), request: vi.fn() };
    const { app, requests } = harness('viewer', false, host); await app.start(); await vi.waitFor(() => expect(document.body.textContent).toContain('本机 Agent'));
    expect(requests).toHaveLength(0); expect(document.querySelector('[data-section=clients]')).toBeNull(); app.navigate('nodes'); expect(document.querySelector('[data-testid=page-title]')?.textContent).toBe('本机模式');
    app.navigate('settings', 'desktop'); await vi.waitFor(() => expect(document.body.textContent).toContain('后台 Agent 独立于窗口运行'));
  });
  it('returns from local mode to login without a server request and offers the local Controller address', async () => {
    const host: Host = { desktop: true, call: vi.fn(async action => action === 'preferences' ? { local_mode: true } : null), request: vi.fn() };
    const { app, requests } = harness('owner', false, host); await app.start();
    await vi.waitFor(() => expect(document.querySelector('[data-testid=local-login-entry]')).not.toBeNull());
    document.querySelector<HTMLButtonElement>('[data-testid=local-login-entry]')!.click();
    await vi.waitFor(() => expect(document.querySelector('#login-name')).not.toBeNull());
    expect(requests).toHaveLength(0);
    document.querySelector<HTMLButtonElement>('[data-testid=login-local-controller]')!.click();
    expect(document.querySelector<HTMLInputElement>('#login-server')!.value).toBe('http://127.0.0.1:9674');
    expect(document.activeElement).toBe(document.querySelector('#login-name'));
  });
  it('rejects inherited or malformed restored routes and ignores late page loading after disposal', async () => {
    const host: Host = { desktop: true, call: vi.fn().mockResolvedValue({}), request: vi.fn() };
    localStorage.setItem('rustfrp.page', JSON.stringify({ section: 'toString' })); const first = harness('viewer', true, host); await first.app.start(); await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6));
    first.app.dispose(); localStorage.setItem('rustfrp.page', 'not-json'); const second = harness('viewer', true, host); await second.app.start(); expect(localStorage.getItem('rustfrp.page')).toBeNull(); second.app.navigate('tunnels'); second.app.dispose(); await Promise.resolve(); expect(document.getElementById('app')!.children).toHaveLength(0);
  });
  it('continues a configured but unenrolled local computer from login directly to node selection', async () => {
    const host: Host = { desktop: true, call: vi.fn(async (action): Promise<Json> => action === 'preferences' ? { local_mode: true, local: { binary: '/agent', stateDir: '/identity', controller: '127.0.0.1:7000' } } : action === 'info' ? { local_enrolled: false, local_defaults: { binary: '/agent', stateDir: '/identity', name: '本机' } } : null), request: vi.fn() };
    const { app, requests } = harness('owner', false, host); await app.start();
    await vi.waitFor(() => expect(document.querySelector('[data-testid=local-start]')).not.toBeNull());
    document.querySelector<HTMLButtonElement>('[data-testid=local-start]')!.click();
    await vi.waitFor(() => expect(document.querySelector('#login-name')).not.toBeNull());
    expect(document.querySelector<HTMLInputElement>('#login-server')!.value).toBe('http://127.0.0.1:9674');
    document.querySelector<HTMLInputElement>('#login-name')!.value = 'preview'; document.querySelector<HTMLInputElement>('#login-password')!.value = 'test password';
    document.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }));
    await vi.waitFor(() => expect(document.querySelector('dialog select[name=node]')).not.toBeNull());
    expect(document.querySelector('[data-testid=page-title]')!.textContent).toBe('本机模式');
    expect(document.querySelector('dialog [name=enrollToken]')).toBeNull(); expect(requests.some(request => request.path.endsWith('/enroll-tokens'))).toBe(false);
    document.querySelector<HTMLButtonElement>('[data-testid=dialog-cancel]')!.click();
  });
  it('finishes persisting local mode before exposing a login that selects a Controller', async () => {
    let save!: (value: Json) => void;
    const host: Host = { desktop: true, call: vi.fn(async (action): Promise<Json> => action === 'preferences' ? { local_mode: true } : action === 'savePreferences' ? new Promise(resolve => { save = resolve; }) : action === 'localStatus' ? null : {}), request: vi.fn() };
    const { app } = harness('owner', false, host); await app.start(); await vi.waitFor(() => { const action = document.querySelector<HTMLButtonElement>('[data-testid=local-start]'); expect(action).toBeDefined(); expect(action!.disabled).toBe(false); });
    document.querySelector<HTMLButtonElement>('[data-testid=local-start]')!.click(); await vi.waitFor(() => expect(save).toBeDefined());
    expect(document.querySelector('#login-name')).toBeNull(); save(null);
    await vi.waitFor(() => expect(document.querySelector('#login-name')).not.toBeNull()); expect(host.call).not.toHaveBeenCalledWith('selectController', expect.anything());
  });
  it('shows a permission failure after successful local login in the current page', async () => {
    const host: Host = { desktop: true, call: vi.fn(async (action): Promise<Json> => action === 'preferences' ? { local_mode: true } : action === 'info' ? { local_enrolled: false } : null), request: vi.fn() };
    const { app } = harness('viewer', false, host); await app.start();
    await vi.waitFor(() => expect(document.querySelector('[data-testid=local-start]')).not.toBeNull()); document.querySelector<HTMLButtonElement>('[data-testid=local-start]')!.click();
    await vi.waitFor(() => expect(document.querySelector('#login-name')).not.toBeNull()); document.querySelector<HTMLInputElement>('#login-name')!.value = 'reader'; document.querySelector<HTMLInputElement>('#login-password')!.value = 'test password';
    document.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }));
    await vi.waitFor(() => expect(document.querySelector('.toast.danger')?.textContent).toContain('当前账号没有接入电脑的权限'));
    expect(document.querySelector('[data-testid=page-title]')?.textContent).toBe('本机模式'); expect(document.querySelector('dialog')).toBeNull();
  });
  it('native login selection and login errors remain actionable without storing passwords', async () => {
    const host: Host = { desktop: true, call: vi.fn().mockResolvedValue({}), request: vi.fn() }; const { app, fetcher } = harness('owner', false, host); await app.start();
    (document.querySelector('#login-server') as HTMLInputElement).value = 'https://controller.example.com'; (document.querySelector('#login-name') as HTMLInputElement).value = 'root'; (document.querySelector('#login-password') as HTMLInputElement).value = 'private test password';
    fetcher.mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: 'invalid_credentials', message: '密码无效' } }), { status: 401 }));
    document.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true })); await vi.waitFor(() => expect(document.querySelector('.form-error')!.textContent).toBe('密码无效')); expect(host.call).toHaveBeenCalledWith('selectController', { url: 'https://controller.example.com' }); expect(localStorage.getItem('rustfrp.server')).toBe('https://controller.example.com'); expect(JSON.stringify(localStorage)).not.toContain('private test password');
    (document.querySelector('[data-testid=login-local]') as HTMLButtonElement).click(); await vi.waitFor(() => expect(document.querySelector('[data-testid=page-title]')!.textContent).toBe('本机模式'));
  });
  it('restores a browser session and exposes matching navigation and compact real metrics', async () => {
    const { app } = harness(); await app.start(); await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6));
    expect(document.body.textContent).toContain('概览'); expect(document.querySelector('[data-section=audit]')).not.toBeNull();
    (document.querySelector('.collapse') as HTMLButtonElement).click(); expect(document.documentElement.dataset.collapsed).toBe('true');
    app.navigate('clients'); await vi.waitFor(() => expect(document.body.textContent).toContain('中文 NAS'));
    (document.querySelector('[data-testid=detail-c]') as HTMLButtonElement).click(); await vi.waitFor(() => expect(document.body.textContent).toContain('返回列表'));
    app.navigate('tunnels'); await vi.waitFor(() => expect(document.body.textContent).toContain('暂无记录'));
    app.navigate('settings'); await vi.waitFor(() => expect(document.body.textContent).toContain('当前账号'));
    const theme = [...document.querySelectorAll('button')].find(button => button.textContent === '深色')!; theme.click(); expect(document.documentElement.dataset.theme).toBe('dark');
    const logout = [...document.querySelectorAll('button')].find(button => button.textContent === '退出登录')!; logout.click(); await vi.waitFor(() => expect(document.querySelector('#login-name')).not.toBeNull());
  });
  it('does not show privileged navigation to a viewer or send an unauthorized route', async () => {
    const { app } = harness('viewer'); await app.start(); expect(document.querySelector('[data-section=audit]')).toBeNull();
    app.navigate('audit'); expect(document.querySelector('[data-testid=page-title]')?.textContent).toBe('概览');
  });
  it('logs in using a real form, prevents duplicate submission and clears password values', async () => {
    const { app, requests } = harness('owner', false); await app.start(); expect(document.querySelector('#login-server')).toBeNull();
    (document.querySelector('#login-name') as HTMLInputElement).value = 'root'; const password = document.querySelector('#login-password') as HTMLInputElement; password.value = 'test password';
    const form = document.querySelector('form')!; form.dispatchEvent(new Event('submit', { cancelable: true })); form.dispatchEvent(new Event('submit', { cancelable: true }));
    await vi.waitFor(() => expect(document.querySelector('.app-shell')).not.toBeNull()); expect(requests.filter(request => request.path.endsWith('/auth/login'))).toHaveLength(1); expect(password.value).toBe('');
  });
  it('reports offline reads without discarding the last valid screen', async () => {
    const { app, fetcher } = harness(); await app.start(); await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6));
    fetcher.mockRejectedValue(new Error('offline')); (document.querySelector('[data-testid=refresh-page]') as HTMLButtonElement).click();
    await vi.waitFor(() => expect(document.querySelector('.toast.danger')).not.toBeNull()); expect(document.querySelectorAll('.metric-card')).toHaveLength(6);
  });
});
