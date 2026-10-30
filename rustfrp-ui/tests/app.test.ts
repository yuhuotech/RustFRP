import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../src/app/app';
import { ApiClient } from '../src/api/client';
import { WebHost, type Host } from '../src/platform/host';
import type { Json } from '../src/api/models';
const apps: App[] = [];
afterEach(() => { for (const app of apps.splice(0)) app.dispose(); vi.useRealTimers(); });
function harness(authenticated = false, host: Host = new WebHost(), currentRole = 'owner') {
  let signedIn = authenticated;
  const fetcher = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit): Promise<Response> => {
    const path = String(input).split('?')[0];
    if (path.endsWith('/auth/login')) { signedIn = true; return new Response(JSON.stringify({ csrf: 'test', admin: { id: '__management__', name: '管理 token', role: currentRole } })); }
    if (path.endsWith('/auth/logout')) { signedIn = false; return new Response('{}'); }
    if (!signedIn) return new Response(JSON.stringify({ error: { code: 'unauthorized', message: '请连接管理服务器' } }), { status: 401 });
    const data: Record<string, Json> = {
      '/api/v1/auth/me': { csrf: 'test', admin: { id: '__management__', name: '管理 token', role: currentRole } },
      '/api/v1/overview': { counts: { tunnels_running: 0, tokens_pending: 0 }, clients_online: 1, traffic: {}, traffic_24h: {}, process: {}, version: '0.1.0' },
      '/api/v1/nodes': [], '/api/v1/clients': [], '/api/v1/tunnels': [],
      '/api/v1/traffic': { range_hours: 24, window_end: 1700000000000, bucket_ms: 3600000, items: [] },
    };
    return new Response(JSON.stringify(data[path] ?? {}));
  });
  const app = new App(document.getElementById('app')!, host, new ApiClient(undefined, fetcher)); apps.push(app); return { app, fetcher };
}
const input = (name: string) => document.querySelector<HTMLInputElement>(`#login-${name}`)!;
const submit = () => document.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }));
describe('single management token console', () => {
  it('sends only the token, prevents duplicate login and never stores credentials', async () => {
    const { app, fetcher } = harness(); await app.start(); expect(input('name')).toBeNull(); expect(input('password')).toBeNull(); expect(input('server').value).toBe(location.origin);
    expect(document.querySelector('[data-testid=login-local]')).toBeNull(); expect(document.querySelector<HTMLElement>('.form-error')!.hidden).toBe(true); const secret = input('token'); secret.value = 'test-management-token'; submit(); submit();
    await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6)); const calls = fetcher.mock.calls.filter(([path]) => String(path).endsWith('/auth/login'));
    expect(calls).toHaveLength(1); expect(JSON.parse(String(calls[0]![1]?.body))).toEqual({ token: 'test-management-token' }); expect(secret.value).toBe(''); expect(JSON.stringify(localStorage)).not.toContain('test-management-token');
    expect(document.querySelector('.nav-foot [data-section=local]')).toBeNull();
  });
  it('ignores old local-mode startup and selects the local management service', async () => {
    const host: Host = { desktop: true, call: vi.fn().mockResolvedValue({ local_mode: true }), request: vi.fn() }; const { app } = harness(false, host); await app.start();
    expect(input('server').value).toBe('http://127.0.0.1:9674'); expect(host.call).toHaveBeenCalledWith('selectController', { url: 'http://127.0.0.1:9674' });
    input('token').value = 'test-token'; submit(); await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6)); expect(host.call).not.toHaveBeenCalledWith('localStart', expect.anything());
    app.navigate('clients'); expect(host.call).not.toHaveBeenCalledWith('savePreferences', expect.objectContaining({ local_mode: expect.anything() }));
  });
  it('restores server and appearance while refusing an old local route', async () => {
    const host: Host = { desktop: true, call: vi.fn().mockResolvedValue({ theme: 'dark', sidebar_collapsed: true, controller_url: 'https://controller.example.com', local_mode: true }), request: vi.fn() };
    localStorage.setItem('rustfrp.page', JSON.stringify({ section: 'local' })); const { app } = harness(true, host); await app.start(); await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6));
    expect(document.documentElement.dataset.theme).toBe('dark'); const toggle = document.querySelector<HTMLButtonElement>('.collapse')!; expect(toggle.querySelector('svg')).not.toBeNull(); expect(toggle.getAttribute('aria-label')).toBe('展开菜单');
    toggle.click(); expect(toggle.getAttribute('aria-expanded')).toBe('true'); expect(host.call).toHaveBeenCalledWith('savePreferences', { sidebar_collapsed: false });
    app.navigate('settings'); await vi.waitFor(() => expect(document.body.textContent).toContain('单管理 token')); expect(document.querySelector('[data-testid=setting-admins]')).toBeNull(); expect(document.querySelector('[data-testid=setting-api]')).toBeNull();
  });
  it('reports wrong tokens, invalid addresses and connection failures in the form', async () => {
    const host: Host = { desktop: true, call: vi.fn().mockResolvedValue({}), request: vi.fn() }; const { app, fetcher } = harness(false, host); await app.start(); input('server').value = 'https://other.example'; input('token').value = 'wrong-token';
    fetcher.mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: 'invalid_credentials', message: '管理 token 不正确' } }), { status: 401 })); submit(); await vi.waitFor(() => expect(document.querySelector('.form-error')!.textContent).toContain('管理 token 不正确'));
    expect(host.call).toHaveBeenCalledWith('selectController', { url: 'https://other.example' }); expect(JSON.stringify(localStorage)).not.toContain('wrong-token');
    input('server').value = 'file:///etc/passwd'; submit(); await vi.waitFor(() => expect(document.querySelector('.form-error')!.textContent).toContain('有效的 HTTP(S)'));
  });
  it('pauses polling while hidden or editing and clears resources on expiry', async () => {
    vi.useFakeTimers(); const { app, fetcher } = harness(true); await app.start(); await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6)); const count = fetcher.mock.calls.length;
    Object.defineProperty(document, 'hidden', { value: true, configurable: true }); await vi.advanceTimersByTimeAsync(5000); expect(fetcher.mock.calls.length).toBe(count);
    Object.defineProperty(document, 'hidden', { value: false, configurable: true }); const dialog = document.createElement('dialog'); dialog.setAttribute('open', ''); document.body.append(dialog);
    await vi.advanceTimersByTimeAsync(5000); expect(fetcher.mock.calls.length).toBe(count); dialog.remove(); document.dispatchEvent(new Event('visibilitychange')); await vi.advanceTimersByTimeAsync(0); expect(fetcher.mock.calls.length).toBeGreaterThan(count);
    app.api.onUnauthorized(); expect(input('token')).not.toBeNull(); expect(app.resources.get('clients')).toBeUndefined();
  });
  it('keeps offline reads visible and disconnects even if the server is unavailable', async () => {
    const { app, fetcher } = harness(true); await app.start(); await vi.waitFor(() => expect(document.querySelectorAll('.metric-card')).toHaveLength(6)); fetcher.mockRejectedValue(new Error('offline')); document.querySelector<HTMLButtonElement>('[data-testid=refresh-page]')!.click();
    await vi.waitFor(() => expect(document.querySelector('.toast.danger')).not.toBeNull()); app.navigate('settings'); await vi.waitFor(() => expect(document.body.textContent).toContain('断开连接'));
    [...document.querySelectorAll('button')].find(button => button.textContent === '断开连接')!.click(); await vi.waitFor(() => expect(input('token')).not.toBeNull());
  });
  it('reports preference failures and discards malformed restored pages', async () => {
    const host: Host = { desktop: true, call: vi.fn().mockRejectedValue(new Error('无法读取偏好')), request: vi.fn() }; const first = harness(false, host); await first.app.start(); expect(document.querySelector('.form-error')!.textContent).toContain('无法读取偏好'); first.app.dispose();
    localStorage.setItem('rustfrp.page', 'invalid'); const second = harness(true, { ...host, call: vi.fn().mockResolvedValue({}) }); await second.app.start(); expect(localStorage.getItem('rustfrp.page')).toBeNull(); second.app.dispose();
  });
  it('retains every management section, retry action and theme across navigation', async () => {
    const { app } = harness(true); await app.start();
    for (const section of ['nodes', 'clients', 'tunnels', 'audit', 'settings', 'overview'] as const) {
      document.querySelector<HTMLButtonElement>(`.nav-item[data-section=${section}]`)!.click();
      await vi.waitFor(() => expect(document.querySelector('[data-testid=refresh-page]')).not.toBeNull());
      await vi.waitFor(() => expect(document.body.textContent).not.toContain('正在加载页面'));
    }
    app.navigate('settings', 'appearance'); await vi.waitFor(() => expect(document.body.textContent).toContain('跟随系统'));
    [...document.querySelectorAll('button')].find(button => button.textContent === '深色')!.click(); expect(document.documentElement.dataset.theme).toBe('dark');
    document.querySelector<HTMLButtonElement>('.collapse')!.click(); expect(document.documentElement.dataset.collapsed).toBe('true');
    document.querySelector<HTMLButtonElement>('.collapse')!.click(); expect(document.documentElement.dataset.collapsed).toBe('false');
  });
  it('rejects unsigned management navigation and legacy lower privilege audit access', async () => {
    const first = harness(); await first.app.start(); first.app.navigate('nodes'); expect(document.querySelector('.app-shell')).toBeNull(); first.app.dispose();
    const second = harness(true, new WebHost(), 'viewer'); await second.app.start(); second.app.navigate('audit'); expect(document.querySelector('[data-testid=page-title]')!.textContent).toBe('概览');
    expect(document.querySelector('[data-section=audit]')).toBeNull();
  });
  it('restores valid desktop sections and ignores inherited or inaccessible route names', async () => {
    const host: Host = { desktop: true, call: vi.fn().mockResolvedValue({}), request: vi.fn() };
    for (const section of ['nodes','clients','tunnels','settings','audit','toString']) {
      localStorage.setItem('rustfrp.page', JSON.stringify({ section, id: 'x' })); const h = harness(true, host); await h.app.start();
      await vi.waitFor(() => expect(document.querySelector('[data-testid=refresh-page]')).not.toBeNull());
      expect(document.querySelector('[data-testid=page-title]')?.textContent).toBe(section === 'toString' ? '概览' : {nodes:'节点管理',clients:'客户端',tunnels:'隧道管理',settings:'系统设置',audit:'审计日志'}[section]);
      h.app.dispose();
    }
    localStorage.setItem('rustfrp.page', JSON.stringify({section:'audit',id:'x'.repeat(257)})); const h=harness(true,host,'viewer'); await h.app.start(); expect(document.querySelector('[data-testid=page-title]')?.textContent).toBe('概览');
  });
  it('drops a token before leaving for another Web control panel and sends no cross-site login', async () => {
    const { app, fetcher } = harness(); await app.start(); input('server').value = 'https://other.example/panel/'; const secret=input('token'); secret.value='never-send-cross-site'; submit();
    await vi.waitFor(() => expect(secret.value).toBe('')); expect(fetcher.mock.calls.some(([url]) => String(url).endsWith('/auth/login'))).toBe(false);
  });
  it('goes to a remote Web console before asking for its token', async () => {
    const {app}=harness(); await app.start(); input('token').value='local-only'; input('server').value='https://other.example/'; input('server').dispatchEvent(new Event('input'));
    expect(input('token').disabled).toBe(true); expect(input('token').value).toBe(''); expect(document.querySelector('[data-testid=login-submit]')!.textContent).toBe('前往管理服务器');
    input('server').value=location.origin; input('server').dispatchEvent(new Event('input')); expect(input('token').required).toBe(true); expect(input('token').disabled).toBe(false);
    input('server').value='invalid-url'; input('server').dispatchEvent(new Event('input')); expect(input('token').disabled).toBe(false);
  });
  it('keeps local maintenance in advanced desktop settings and clears an expired dialog', async () => {
    const host: Host = { desktop:true, call:vi.fn(async(action):Promise<Json>=>action==='preferences'?{}:action==='localStatus'?null:action==='startup'?false:action==='info'?{local_enrolled:false,local_defaults:{binary:'/agent',stateDir:'/identity'}}:{}),request:vi.fn() };
    const { app }=harness(true,host); await app.start(); app.navigate('settings','desktop'); await vi.waitFor(()=>expect(document.querySelector('[data-testid=local-maintenance]')).not.toBeNull());
    document.querySelector<HTMLButtonElement>('[data-testid=local-maintenance]')!.click(); await vi.waitFor(()=>expect(document.body.textContent).toContain('此电脑尚未接入'));
    const dialog=document.createElement('dialog'); dialog.showModal(); document.body.append(dialog); app.api.onUnauthorized(); expect(dialog.open).toBe(false); expect(input('token')).not.toBeNull();
  });
  it('surfaces failed desktop preference writes and does not render a late page after disposal', async () => {
    const host: Host = { desktop:true,call:vi.fn().mockResolvedValue({}),request:vi.fn() }; const { app }=harness(true,host); await app.start();
    vi.mocked(host.call).mockRejectedValue(new Error('preferences unavailable')); document.querySelector<HTMLButtonElement>('.collapse')!.click(); await vi.waitFor(()=>expect(document.querySelector('.toast.danger')).not.toBeNull());
    app.navigate('settings'); await vi.waitFor(()=>expect(document.querySelector('[data-testid=switch-controller]')).not.toBeNull()); document.querySelector<HTMLButtonElement>('[data-testid=switch-controller]')!.click(); await vi.waitFor(()=>expect(input('token')).not.toBeNull());
    app.navigate('settings'); app.dispose(); await Promise.resolve(); expect(document.getElementById('app')!.children).toHaveLength(0);
  });
});
