import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiClient, ApiError } from '../src/api/client';
import type { Json, RecordValue } from '../src/api/models';
import { Resources } from '../src/app/resources';
import type { Context, Page } from '../src/app/context';
import type { HostAction } from '../src/platform/host';
import { localConfigEditor, localSettings } from '../src/features/local-config';
import { localPage } from '../src/features/local';
import fixture from '../../rustfrp-protocol/fixtures/local-status.json';

const active: { context: Context; page?: Page }[] = [];
afterEach(() => {
  for (const dialog of document.querySelectorAll<HTMLDialogElement>('dialog')) dialog.close();
  for (const item of active.splice(0)) { item.page?.dispose(); item.context.resources.dispose(); item.context.api.close(); }
});
function harness(desktop = true) {
  let status: Json = null;
  const prefs: RecordValue = { local: { binary: '/test/agent', stateDir: '/test/state', configFile: '/test/frpc.toml', controller: 'host:7000', name: '本机' } };
  const calls: [HostAction, RecordValue][] = [];
  const call = vi.fn(async (action: HostAction, args: RecordValue = {}): Promise<Json> => {
    calls.push([action, args]);
    if (action === 'preferences') return prefs;
    if (action === 'info') return { local_defaults: { binary: '/test/agent', stateDir: '/test/state', name: '本机' } };
    if (action === 'localStatus') return status;
    if (action === 'readConfig') return { exists: true, text: 'serverAddr="127.0.0.1"\nserverPort=7000', digest: 'original-digest' };
    if (action === 'verifyConfig') return { digest: 'preview-digest', warnings: [], proxies: 0, visitors: 0 };
    if (action === 'saveConfig' || action === 'savePreferences') return { ok: true };
    return null;
  });
  const api = new ApiClient(undefined, vi.fn()); const resources = new Resources(api);
  const context: Context = { api, resources, host: { desktop, call, request: vi.fn() }, admin: null, navigate: vi.fn(), refresh: vi.fn().mockResolvedValue(undefined), logout: vi.fn(), showError: vi.fn(), write: vi.fn() };
  active.push({ context }); return { context, calls, prefs, call, setStatus(value: Json) { status = value; } };
}
it('new local preferences use bundle defaults, preserve managed identity fields and reject a blank standalone path', async () => {
  const h = harness(); const base = h.call.getMockImplementation()!;
  h.call.mockImplementation(async (action, args = {}) => action === 'preferences' ? {} : base(action, args));
  await localSettings(h.context); expect(document.querySelector<HTMLInputElement>('[name=binary]')!.value).toBe('/test/agent'); expect(document.querySelector<HTMLInputElement>('[name=name]')!.value).toBe('本机');
  field('mode', 'standalone'); field('configFile', ''); submit(); await vi.waitFor(() => expect(document.querySelector('.form-error')!.textContent).toContain('独立配置文件路径')); expect(h.calls.some(([action]) => action === 'savePreferences')).toBe(false);
  field('configFile', '/new/frpc.toml'); submit(); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); expect(h.calls.find(([action]) => action === 'savePreferences')?.[1]).toMatchObject({ local: { controller: '', name: '', caFile: '', tlsPin: '', configFile: '/new/frpc.toml' } });
  h.call.mockImplementation(async (action, args = {}): Promise<Json> => action === 'preferences' ? { local: { controller: 'saved:7000', name: 'saved device', caFile: '/saved/ca', tlsPin: 'ab' } } : action === 'info' ? {} : base(action, args));
  await localSettings(h.context); field('binary', '/agent'); field('stateDir', '/identity'); field('mode', 'standalone'); field('configFile', '/new/config'); submit(); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); expect(h.calls.at(-1)).toMatchObject(['savePreferences', { local: { controller: 'saved:7000', name: 'saved device', caFile: '/saved/ca', tlsPin: 'ab' } }]);
});
it('local verification rejects blank, oversized and incompatible responses and ignores a changed buffer or closed dialog', async () => {
  const h = harness(); const editor = await localConfigEditor(h.context); field('text', ''); click('local-config-verify'); await vi.waitFor(() => expect(editor.error.textContent).toContain('不能为空'));
  field('text', 'x'.repeat(1024 * 1024 + 1)); click('local-config-verify'); await vi.waitFor(() => expect(editor.error.textContent).toContain('不能超过')); field('text', 'serverPort=7000'); h.call.mockResolvedValueOnce({ warnings: [] }); click('local-config-verify'); await vi.waitFor(() => expect(editor.error.textContent).toContain('校验响应不兼容'));
  let resolve!: (value: Json) => void; h.call.mockImplementationOnce(() => new Promise(done => { resolve = done; })); click('local-config-verify'); field('text', 'serverPort=7001'); resolve({ digest: 'valid', warnings: [] }); await vi.waitFor(() => expect(document.querySelector<HTMLButtonElement>('[data-testid=local-config-verify]')!.disabled).toBe(false)); expect(editor.submit.disabled).toBe(true); expect(editor.element.querySelector('h3')).toBeNull();
  h.call.mockImplementationOnce(() => new Promise(done => { resolve = done; })); click('local-config-verify'); editor.close(); resolve({ digest: 'valid', warnings: [] }); await Promise.resolve(); expect(document.querySelector('dialog')).toBeNull();
});
function field(name: string, value: string | boolean): void {
  const control = document.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(`dialog [name=${name}]`)!;
  if (typeof value === 'boolean' && control instanceof HTMLInputElement) control.checked = value; else control.value = String(value);
  control.dispatchEvent(new Event('input', { bubbles: true })); control.dispatchEvent(new Event('change', { bubbles: true }));
}
function submit(): void { document.querySelector('dialog form')!.dispatchEvent(new Event('submit', { cancelable: true, bubbles: true })); }
function click(id: string): void { document.querySelector<HTMLButtonElement>(`[data-testid=${id}]`)!.click(); }
async function mounted(h: ReturnType<typeof harness>) {
  const page = localPage(h.context); active.find(item => item.context === h.context)!.page = page; document.getElementById('app')!.append(page.element); await page.refresh(); return page;
}
describe('complete native configuration editor', () => {
  it('retains full text, verifies it, commits with original and preview digests and clears secrets', async () => {
    const h = harness(); const dialog = await localConfigEditor(h.context); const control = dialog.control('text')!;
    expect(control.value).toContain('serverAddr'); expect(dialog.submit.disabled).toBe(true);
    (document.querySelector('[data-testid=local-config-verify]') as HTMLButtonElement).click(); await vi.waitFor(() => expect(dialog.submit.disabled).toBe(false));
    submit(); await vi.waitFor(() => expect(dialog.element.isConnected).toBe(false));
    expect(h.calls.find(([action]) => action === 'saveConfig')).toEqual(['saveConfig', { text: 'serverAddr="127.0.0.1"\nserverPort=7000', original_digest: 'original-digest', preview_digest: 'preview-digest', accept_warnings: true }]);
    expect(control.value).toBe('');
  });
  it('invalidates preview after edits and requires acknowledgment when compatibility warnings remain', async () => {
    const h = harness(); h.call.mockImplementation(async (action: HostAction): Promise<Json> => action === 'readConfig' ? { text: 'old config', digest: 'old' } : action === 'verifyConfig' ? { digest: 'preview', warnings: ['transport.protocol 暂不支持'], proxies: 1, visitors: 0 } : null);
    const dialog = await localConfigEditor(h.context); const verify = document.querySelector<HTMLButtonElement>('[data-testid=local-config-verify]')!; verify.click(); await vi.waitFor(() => expect(dialog.content.textContent).toContain('暂不支持'));
    expect(dialog.submit.disabled).toBe(true); field('accept_warnings', true); expect(dialog.submit.disabled).toBe(false);
    field('text', 'modified config'); expect(dialog.submit.disabled).toBe(true); expect(dialog.content.textContent).toContain('重新校验'); dialog.close();
  });
  it('reports verification failures, ignores stale async results and prevents repeat requests', async () => {
    const h = harness(); const dialog = await localConfigEditor(h.context); const verify = document.querySelector<HTMLButtonElement>('[data-testid=local-config-verify]')!;
    h.call.mockRejectedValueOnce(new ApiError(0, 'invalid', '配置格式错误')); verify.click(); await vi.waitFor(() => expect(dialog.error.textContent).toContain('配置格式错误')); expect(dialog.submit.disabled).toBe(true);
    let resolve!: (value: Json) => void; h.call.mockImplementationOnce(() => new Promise<Json>(done => { resolve = done; })); verify.click(); verify.click(); expect(verify.disabled).toBe(true);
    field('text', 'newer config'); resolve({ digest: 'stale', warnings: [], proxies: 0, visitors: 0 }); await vi.waitFor(() => expect(verify.disabled).toBe(false)); expect(dialog.submit.disabled).toBe(true);
    field('text', ''); verify.click(); await vi.waitFor(() => expect(dialog.error.textContent).toContain('不能为空')); dialog.close();
  });
  it('keeps mode settings separate from credentials and retains inactive connection preferences', async () => {
    const h = harness(); await localSettings(h.context); expect(document.querySelector<HTMLElement>('fieldset')?.hidden).toBe(true);
    field('mode', 'managed'); field('controller', 'next.example.com:7000'); field('name', '工作站'); submit(); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull());
    const call = h.calls.find(([action]) => action === 'savePreferences'); expect(call?.[1]).toEqual({ local: { binary: '/test/agent', stateDir: '/test/state', configFile: '', controller: 'next.example.com:7000', name: '工作站', caFile: '', tlsPin: '' } }); expect(JSON.stringify(call)).not.toContain('enrollToken');
  });
});
describe('local status reads stay bounded and platform-aware', () => {
  it('shows the shared local-control fixture and avoids rebuilding an unchanged status', async () => {
    const h = harness(); h.setStatus(fixture); const page = localPage(h.context); active.find(item => item.context === h.context)!.page = page; document.getElementById('app')!.append(page.element); await page.refresh();
    expect(page.element.textContent).toContain('进程 PID'); expect(page.element.textContent).toContain(String(fixture.pid)); const panel = page.element.querySelector('.panel'); await page.refresh(); expect(page.element.querySelector('.panel')).toBe(panel);
  });
  it('shows missing discovery without inventing a healthy status, and Web never calls private control', async () => {
    const native = harness(); const page = localPage(native.context); active.find(item => item.context === native.context)!.page = page; document.body.append(page.element); await page.refresh(); expect(page.element.textContent).toContain('未发现本机 Agent'); expect(document.querySelector('[data-testid=local-config-edit]')).not.toBeNull();
    const browser = harness(false); const web = localPage(browser.context); active.find(item => item.context === browser.context)!.page = web; await web.refresh(); expect(web.element.textContent).toContain('需要桌面客户端'); expect(browser.call).not.toHaveBeenCalled();
  });
});
describe('native Agent actions and applied data', () => {
  it('uses the enrollment token once, blocks repeated submission and clears the field on success', async () => {
    const h = harness(); recordLocal(h.prefs).configFile = ''; const page = await mounted(h);
    click('local-start'); field('enrollToken', 'dummy-once'); const token = document.querySelector<HTMLInputElement>('dialog input[name=enrollToken]')!;
    const base = h.call.getMockImplementation()!; let resolve!: (value: Json) => void;
    h.call.mockImplementation((action, args) => action === 'localStart' ? new Promise<Json>(done => { resolve = done; }) : base(action, args));
    submit(); submit(); expect(h.call.mock.calls.filter(([action]) => action === 'localStart')).toHaveLength(1); expect(document.querySelector<HTMLButtonElement>('[data-testid=dialog-cancel]')!.disabled).toBe(true);
    expect(h.call).toHaveBeenCalledWith('localStart', { enrollToken: 'dummy-once' }); expect(JSON.stringify(h.prefs)).not.toContain('dummy-once');
    resolve(fixture); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); expect(token.value).toBe(''); expect(h.context.refresh).toHaveBeenCalledTimes(1);
    h.setStatus(fixture); await page.refresh(); expect(document.querySelector<HTMLButtonElement>('[data-testid=local-start]')!.disabled).toBe(true); expect(document.querySelector<HTMLButtonElement>('[data-testid=local-settings]')!.disabled).toBe(true);
  });
  it('starts standalone mode without credentials, stops with confirmation and restores action eligibility', async () => {
    const h = harness(); const page = await mounted(h); click('local-start'); expect(document.querySelector('dialog input[name=enrollToken]')).toBeNull(); submit(); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); expect(h.calls).toContainEqual(['localStart', { enrollToken: '' }]);
    h.setStatus(fixture); await page.refresh(); click('local-stop'); expect(document.querySelector('dialog')!.textContent).toContain('本机隧道将中断'); submit(); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); expect(h.calls).toContainEqual(['localStop', {}]);
    h.setStatus(null); await page.refresh(); expect(document.querySelector<HTMLButtonElement>('[data-testid=local-stop]')!.disabled).toBe(true); expect(document.querySelector<HTMLButtonElement>('[data-testid=local-start]')!.disabled).toBe(false);
  });
  it('shows uncertain start failure and prevents another submission until the user refreshes', async () => {
    const h = harness(); recordLocal(h.prefs).configFile = ''; await mounted(h); click('local-start'); field('enrollToken', 'dummy-once');
    h.call.mockRejectedValueOnce(new ApiError(0, 'local_timeout', '启动尚未确认', true)); submit(); await vi.waitFor(() => expect(document.querySelector('dialog')!.textContent).toContain('不要重复提交'));
    expect(document.querySelector<HTMLButtonElement>('[data-testid=dialog-submit]')!.disabled).toBe(true); const attempts = h.call.mock.calls.length; submit(); expect(h.call.mock.calls).toHaveLength(attempts); click('dialog-cancel'); expect(document.querySelector('dialog')).toBeNull();
  });
  it('shows real applied tunnel addresses and revisions and retains an unchanged resource row', async () => {
    const h = harness(); h.setStatus(fixture); const base = h.call.getMockImplementation()!;
    let revision = 7; h.call.mockImplementation((action, args) => action === 'localTunnels' ? Promise.resolve({ revision, items: [{ id: 'tcp-1', kind: 'tcp', local_ip: '127.0.0.1', local_port: 8080, remote_port: 0 }] }) : base(action, args));
    const page = await mounted(h); click('local-tab-tunnels'); await vi.waitFor(() => expect(page.element.textContent).toContain('127.0.0.1:8080')); expect(page.element.textContent).toContain('由服务端分配'); const row = page.element.querySelector('[data-row-id=tcp-1]');
    revision = 8; await page.refresh(); expect(page.element.textContent).toContain('配置版本 8'); expect(page.element.querySelector('[data-row-id=tcp-1]')).toBe(row);
  });
  it('reads incremental logs, avoids duplicate lines and exposes ring eviction', async () => {
    const h = harness(); h.setStatus(fixture); const base = h.call.getMockImplementation()!; const argsSeen: RecordValue[] = [];
    h.call.mockImplementation((action, args = {}) => { if (action !== 'localLogs') return base(action, args); argsSeen.push(args); return Promise.resolve(argsSeen.length === 1 ? { cursor: 1, gap: false, items: ['first'], capacity: 128 } : argsSeen.length === 2 ? { cursor: 1, gap: false, items: [], capacity: 128 } : { cursor: 2, gap: true, items: ['second'], capacity: 128 }); });
    const page = await mounted(h); click('local-tab-logs'); await vi.waitFor(() => expect(page.element.textContent).toContain('first')); await page.refresh(); expect(argsSeen[1]).toEqual({ after: 1 }); expect(page.element.querySelectorAll('.log-line')).toHaveLength(1);
    await page.refresh(); expect(page.element.textContent).toContain('环形缓冲覆盖'); expect(page.element.textContent).toContain('second');
  });
  it('keeps legacy snapshots bounded without duplicating them and exports diagnostics with clear failure feedback', async () => {
    const h = harness(); h.setStatus(fixture); const base = h.call.getMockImplementation()!;
    h.call.mockImplementation((action, args) => action === 'localLogs' ? Promise.resolve({ items: ['legacy line'] }) : action === 'exportDiagnostics' ? Promise.resolve({ path: '/test/state/diagnostics.json' }) : base(action, args));
    const page = await mounted(h); click('local-tab-logs'); await vi.waitFor(() => expect(page.element.textContent).toContain('日志快照')); await page.refresh(); expect(page.element.querySelectorAll('.log-line')).toHaveLength(1);
    click('local-diagnostics'); await vi.waitFor(() => expect(document.getElementById('notifications')!.textContent).toContain('/test/state/diagnostics.json'));
    h.call.mockResolvedValueOnce({}); click('local-diagnostics'); await vi.waitFor(() => expect(h.context.showError).toHaveBeenCalled());
  });
  it('ignores data from a tab disposed while an async request is still running', async () => {
    const h = harness(); h.setStatus(fixture); const page = await mounted(h); const base = h.call.getMockImplementation()!; let resolve!: (value: Json) => void;
    h.call.mockImplementation((action, args) => action === 'localTunnels' ? new Promise<Json>(done => { resolve = done; }) : base(action, args));
    click('local-tab-tunnels'); await vi.waitFor(() => expect(resolve).toBeDefined()); page.dispose(); resolve({ revision: 99, items: [{ id: 'stale', kind: 'tcp', local_ip: 'localhost', local_port: 1, remote_port: 2 }] });
    await Promise.resolve(); await Promise.resolve(); expect(page.element.textContent).not.toContain('stale'); expect(page.element.textContent).not.toContain('配置版本 99');
  });
});
function recordLocal(prefs: RecordValue): RecordValue { return prefs.local as RecordValue; }
