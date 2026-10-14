import { afterEach, expect, it, vi } from 'vitest';
import { AppUpdate } from '../src/features/app-update';
import type { Context } from '../src/app/context';
import type { Json, RecordValue } from '../src/api/models';
import type { Host, HostAction } from '../src/platform/host';
import { ApiClient } from '../src/api/client';
import { Resources } from '../src/app/resources';

const alive: { panel: AppUpdate; context: Context }[] = [];
afterEach(() => { vi.useRealTimers(); for (const dialog of document.querySelectorAll<HTMLDialogElement>('dialog')) dialog.close(); for (const entry of alive.splice(0)) { entry.panel.dispose(); entry.context.resources.dispose(); entry.context.api.close(); } });
function fixture(initial: RecordValue = {}) {
  let state: RecordValue = { phase: 'idle', available: false, downloaded: 0, total: 0, ...initial }, preferences: RecordValue = { update_source: 'github' };
  const release = { version: '0.2.0', page: 'https://github.com/owner/project/releases/tag/v0.2.0', notes: '<script>untrusted release text</script>' };
  const failures = new Set<HostAction>(); let hold: (() => Promise<Json>) | undefined;
  const call = vi.fn(async (action: HostAction, args: RecordValue = {}): Promise<Json> => {
    if (failures.has(action)) throw new Error('fixture native failure');
    if (action === 'preferences') return preferences;
    if (action === 'updateStatus') return state;
    if (action === 'savePreferences') { preferences = { ...preferences, ...args }; return preferences; }
    if (action === 'checkUpdate') { if (hold) return hold(); state = { ...state, available: true, latest: release }; return state; }
    if (action === 'installUpdate') { state = { ...state, phase: 'installing' }; return state; }
    if (action === 'cancelUpdate') { state = { ...state, phase: 'cancelled' }; return state; }
    return null;
  });
  const host: Host = { desktop: true, call, request: vi.fn() }; const api = new ApiClient(undefined, vi.fn()); const resources = new Resources(api);
  const context: Context = { host, api, resources, admin: null, navigate: vi.fn(), refresh: vi.fn(), logout: vi.fn(), write: vi.fn(), showError: vi.fn() };
  const panel = new AppUpdate(context); document.body.append(panel.element); alive.push({ panel, context });
  return { panel, context, call, failures, release, state: (value: RecordValue) => { state = value; }, hold: (value: () => Promise<Json>) => { hold = value; } };
}
const click = (id: string) => document.querySelector<HTMLButtonElement>(`[data-testid="${id}"]`)!.click();
it('checks stable updates, renders release notes as text and requires a concrete install confirmation', async () => {
  const h = fixture(); await h.panel.refresh(); expect(document.body.textContent).toContain('检查最新稳定版本'); click('app-update-check');
  await vi.waitFor(() => expect(document.querySelector('[data-testid=app-update-install]')).not.toBeNull()); expect(document.querySelector('script')).toBeNull(); expect(document.body.textContent).toContain('<script>untrusted release text</script>');
  click('app-update-release'); await vi.waitFor(() => expect(h.call).toHaveBeenCalledWith('openUrl', { url: h.release.page }));
  click('app-update-install'); expect(document.querySelector('dialog')?.textContent).toContain('0.2.0'); expect(h.call).not.toHaveBeenCalledWith('installUpdate');
  document.querySelector('dialog form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
  await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); expect(h.call).toHaveBeenCalledWith('installUpdate'); expect(document.body.textContent).toContain('安装程序已接续'); expect(document.querySelector<HTMLButtonElement>('[data-testid=app-update-check]')!.disabled).toBe(true);
});
it('persists mirror selection and restores the original control on failure', async () => {
  const h = fixture(); await h.panel.refresh(); let select = document.querySelector<HTMLSelectElement>('select')!; select.value = 'mainland'; select.dispatchEvent(new Event('change'));
  await vi.waitFor(() => { expect(document.querySelector<HTMLSelectElement>('select')!.value).toBe('mainland'); expect(document.querySelector<HTMLSelectElement>('select')!.disabled).toBe(false); }); expect(h.call).toHaveBeenCalledWith('savePreferences', { update_source: 'mainland' });
  h.failures.add('savePreferences'); select = document.querySelector<HTMLSelectElement>('select')!; select.value = 'github'; select.dispatchEvent(new Event('change'));
  await vi.waitFor(() => expect(h.context.showError).toHaveBeenCalled()); expect(select.value).toBe('mainland'); expect(select.disabled).toBe(false);
});
it('keeps manual, disabled, current and failed update states explicit', async () => {
  const h = fixture({ disabled: '未配置发布公钥' }); await h.panel.refresh(); expect(document.body.textContent).toContain('未配置发布公钥'); expect(document.querySelector<HTMLButtonElement>('[data-testid=app-update-check]')!.disabled).toBe(true);
  h.state({ available: true, latest: h.release, manual: '请手动安装' }); await h.panel.refresh(); expect(document.body.textContent).toContain('请手动安装'); expect(document.querySelector('[data-testid=app-update-install]')).toBeNull();
  h.state({ latest: h.release, available: false }); await h.panel.refresh(); expect(document.body.textContent).toContain('最新稳定版本');
  h.state({ error: '签名无效' }); await h.panel.refresh(); expect(document.body.textContent).toContain('签名无效'); h.failures.add('checkUpdate'); click('app-update-check'); await vi.waitFor(() => expect(h.context.showError).toHaveBeenCalled()); expect(document.querySelector<HTMLButtonElement>('[data-testid=app-update-check]')!.disabled).toBe(false);
});
it('polls progress, prevents duplicate checks and permits cancellation while a native operation is pending', async () => {
  vi.useFakeTimers(); const h = fixture(); await h.panel.refresh(); let resolve!: (value: Json) => void; h.hold(() => new Promise(done => { resolve = done; }));
  click('app-update-check'); click('app-update-check'); expect(h.call.mock.calls.filter(([action]) => action === 'checkUpdate')).toHaveLength(1);
  h.state({ phase: 'downloading', total: 100, downloaded: 25 }); await vi.advanceTimersByTimeAsync(500); const progress = document.querySelector('progress')!; expect(progress.hidden).toBe(false); expect(progress.value).toBe(25); expect(progress.max).toBe(100);
  click('app-update-cancel'); await vi.advanceTimersByTimeAsync(0); expect(h.call).toHaveBeenCalledWith('cancelUpdate'); resolve({ phase: 'cancelled' }); await vi.advanceTimersByTimeAsync(0); expect(document.body.textContent).toContain('更新已取消');
  h.state({ phase: 'checking', total: -1, downloaded: 'bad' }); await h.panel.refresh(); expect(document.querySelector('progress')!.max).toBe(1); expect(document.querySelector('progress')!.value).toBe(0); expect(document.body.textContent).toContain('正在检查');
});
it('leaving the page clears polling and late responses do not redraw disposed content', async () => {
  vi.useFakeTimers(); const h = fixture(); await h.panel.refresh(); let resolve!: (value: Json) => void; h.hold(() => new Promise(done => { resolve = done; })); click('app-update-check'); const content = h.panel.element.innerHTML; h.panel.dispose(); resolve({ available: true, latest: h.release }); await vi.advanceTimersByTimeAsync(1000); expect(h.panel.element.innerHTML).toBe(content);
  h.state({ available: true }); await h.panel.refresh(); expect(h.panel.element.innerHTML).toBe(content);
});
