import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../src/api/client';
import { FormDialog, parseField, confirmation } from '../src/components/forms';
import { el, keep, button, select, toast, details, value } from '../src/components/dom';
import { ResourceTable } from '../src/components/table';
import { allowed, address, bytes, date, disabled, numeric, publicAddress, record, records, role, status, text, tokenStatus, type Tunnel, type NodeInfo } from '../src/api/models';
import { copy, guarded, View } from '../src/components/operations';
import type { Context } from '../src/app/context';
import { LogView } from '../src/components/logs';

describe('data semantics', () => {
  it('async controls display failures and keep explicit eligibility across an operation', async () => {
    const sync = button('同步失败', () => { throw 'fixture error'; }); document.body.append(sync); sync.click(); expect(document.body.textContent).toContain('fixture error');
    const async = button('异步失败', () => Promise.reject('async fixture')); document.body.append(async); async.click(); await vi.waitFor(() => expect(document.body.textContent).toContain('async fixture')); expect(async.disabled).toBe(false);
    const native = { host: { call: vi.fn().mockResolvedValue(null) }, showError: vi.fn() } as unknown as Context; const copied = copy(native, () => 'private value'); document.body.append(copied); copied.click(); await vi.waitFor(() => { expect(native.host.call).toHaveBeenCalledWith('copy', { text: 'private value' }); expect(copied.disabled).toBe(false); }); expect(document.body.textContent).toContain('已复制'); vi.mocked(native.host.call).mockRejectedValue('clipboard denied'); copied.click(); await vi.waitFor(() => expect(native.showError).toHaveBeenCalledWith('clipboard denied'));
    let resolve!: () => void; const task = vi.fn(() => new Promise<void>(done => { resolve = done; })); const guard = guarded(native, task); const first = guard(); await guard(); expect(task).toHaveBeenCalledOnce(); resolve(); await first;
    const root = el('div'); document.body.append(root); const view = new View(root); view.update({ state: 1 }, () => { const input = el('input'); input.dataset.testid = 'focus'; return [input]; }); root.querySelector('input')!.focus(); view.update({ state: 2 }, () => { const input = el('input'); input.dataset.testid = 'focus'; return [input]; }); expect(document.activeElement).toBe(root.querySelector('input'));
    document.getElementById('notifications')!.remove(); expect(() => toast('absent')).not.toThrow(); const selectControl = select([['a', 'A']], 'a', vi.fn()); expect(selectControl.dataset.testid).toBeUndefined();
  });
  it('log snapshots preserve scroll position and reject oversized or malformed payloads', () => {
    const logs = new LogView(2); document.body.append(logs.element); Object.defineProperty(logs.element, 'scrollHeight', { value: 1000 }); Object.defineProperty(logs.element, 'clientHeight', { value: 100 }); logs.element.scrollTop = 10;
    logs.append({ items: [], cursor: 0 }); expect(logs.element.textContent).toContain('暂无日志'); logs.append({ items: ['a'], cursor: 1 }); expect(logs.element.scrollTop).toBe(10); const line = logs.element.firstChild; logs.append({ items: [], cursor: 1 }); expect(logs.element.firstChild).toBe(line);
    logs.snapshot({ items: [] }); expect(logs.element.textContent).toContain('暂无日志'); expect(() => logs.snapshot({ items: Array(129).fill('x') })).toThrow(); expect(() => logs.snapshot({ items: ['x'.repeat(513)] })).toThrow(); expect(() => logs.append({ items: [], cursor: -1 })).toThrow(); expect(() => logs.append({ items: [], cursor: 1.1 })).toThrow();
  });
  it('does not turn missing data into a healthy zero or elevated privilege', () => {
    expect(numeric(null)).toBeNull(); expect(numeric(Infinity)).toBeNull(); expect(text(null)).toBe('—');
    expect(role('future-role')).toBe('viewer'); expect(allowed('operator', 'admin')).toBe(false); expect(allowed('owner', 'admin')).toBe(true);
    expect(() => record([])).toThrow(); expect(() => records(null)).toThrow(); expect(records([{ id: 'a' }])).toEqual([{ id: 'a' }]);
    expect(disabled({ disabled_at: null })).toBe(false); expect(disabled({ disabled_at: 1 })).toBe(true);
    expect(status({ disabled_at: 1, online: true })).toEqual(['已停用', 'muted']); expect(status({ online: false })).toEqual(['离线', 'muted']);
    expect(status({ online: true })).toEqual(['在线', 'success']); expect(status({ state: 'running' })).toEqual(['运行中', 'success']); expect(status({ state: 'unknown' })).toEqual(['未知', 'muted']);
  });
  it('formats bytes, dates, token states and IPv6 addresses consistently', () => {
    expect(bytes(null)).toBe('—'); expect(bytes(-1)).toBe('—'); expect(bytes(0)).toBe('0 B'); expect(bytes(1536)).toBe('1.5 KiB');
    expect(date(0)).toBe('—'); expect(date(1700000000000)).not.toBe('—'); expect(address('::1', 7000)).toBe('[::1]:7000'); expect(address('host', 0)).toBe('未分配');
    expect(tokenStatus({ revoked_at: 1 })).toEqual(['已撤销', 'muted']); expect(tokenStatus({ used_at: 1 })).toEqual(['已使用', 'success']);
    expect(tokenStatus({ expires_at: 2 }, 3)).toEqual(['已过期', 'muted']); expect(tokenStatus({ expires_at: 4 }, 3)).toEqual(['待使用', 'warning']); expect(tokenStatus({})).toEqual(['未知', 'muted']);
    const node = { public_host: 'host', vhost_http: 8080, vhost_https: 443, subdomain_host: 'example.com', tcpmux_http_connect_port: 5000 } as NodeInfo;
    expect(publicAddress({ kind: 'stcp' } as Tunnel, node)).toContain('私密');
    expect(publicAddress({ kind: 'http', custom_domains: ['example.com'] } as Tunnel, node)).toBe('http://example.com:8080');
    expect(publicAddress({ id: 't', name: 't', client_id: 'c', enabled: true, state: 'running', status_error: '', status_port: 0, local_ip: '127.0.0.1', local_port: 80, remote_port: 0, kind: 'https', custom_domains: [], subdomain: 'x' }, node)).toBe('https://x.example.com');
    expect(publicAddress({ kind: 'tcpmux' } as Tunnel, node)).toBe('host:5000');
    expect(publicAddress({ kind: 'tcp', status_port: 4000 } as Tunnel, node)).toBe('host:4000');
  });
});

describe('DOM identity and bounded tables', () => {
  it('keeps the same focused input and escapes untrusted labels', () => {
    const root = el('div'); const input = el('input'); input.value = '中文输入'; const label = el('span', '', '<img onerror=alert(1)>'); root.append(input, label); document.body.append(root); input.focus();
    keep(root, [label, input]); expect(document.activeElement).toBe(input); expect(input.value).toBe('中文输入'); expect(root.querySelector('img')).toBeNull();
    keep(root, [input]); expect(root.childNodes).toHaveLength(1);
  });
  it('reuses unchanged rows and bounds rendered rows for large data', async () => {
    const table = new ResourceTable(['名称', '操作']); document.body.append(table.element); const callback = vi.fn();
    const row = { id: 'one', signature: 'v1', cells: () => [value('one'), button('详情', callback, { id: 'detail-one' })] };
    table.update([row]); const existing = table.element.querySelector('tr[data-row-id=one]'); table.update([row]); expect(table.element.querySelector('tr[data-row-id=one]')).toBe(existing);
    const action = table.element.querySelector<HTMLButtonElement>('button')!; action.focus(); table.update([{ ...row, signature: 'v2' }]); expect(document.activeElement?.textContent).toBe('详情');
    (document.activeElement as HTMLButtonElement).click(); expect(callback).toHaveBeenCalledOnce();
    table.update(Array.from({ length: 1000 }, (_, index) => ({ id: String(index), signature: String(index), cells: () => [value(String(index)), el('span', '', '查看')] })));
    expect(table.element.querySelectorAll('[data-row-id]').length).toBeLessThan(30); table.element.scrollTop = 4000; table.element.dispatchEvent(new Event('scroll'));
    await new Promise(resolve => requestAnimationFrame(resolve)); expect(table.element.querySelector('[data-row-id="50"]')).not.toBeNull();
    table.update([]); expect(table.element.textContent).toContain('暂无记录'); table.dispose();
  });
  it('provides basic controls with real labels and a bounded notification stack', () => {
    const changed = vi.fn(); const picker = select([['a', '甲'], ['b', '乙']], 'a', changed, 'picker'); picker.value = 'b'; picker.dispatchEvent(new Event('change')); expect(changed).toHaveBeenCalledWith('b');
    for (let i = 0; i < 8; i++) toast(`notice ${i}`, i === 7 ? 'danger' : 'success'); expect(document.querySelectorAll('.toast')).toHaveLength(4); expect(document.querySelector('[role=alert]')?.textContent).toBe('notice 7');
    expect(details([['名称', '服务器']]).textContent).toBe('名称服务器');
  });
  it('shows real numeric and boolean data while preserving missing values', () => {
    expect(value(0, 7000).textContent).toBe('07000');
    expect(value(7000).querySelector<HTMLElement>('.cell-main')?.title).toBe('7000');
    expect(details([['端口', 7000], ['已启用', true], ['自动分配', false], ['缺失', null], ['无效', NaN]]).textContent)
      .toBe('端口7000已启用是自动分配否缺失—无效—');
  });
});

describe('forms, failures and repeat-click protection', () => {
  it('validates numbers and JSON and preserves unset write-only secrets', () => {
    const input = el('input'); input.value = '';
    expect(parseField({ key: 'password', label: '密码', secret: true }, input)).toBeUndefined();
    expect(() => parseField({ key: 'name', label: '名称', required: true }, input)).toThrow('请填写');
    input.value = '-1'; expect(() => parseField({ key: 'port', label: '端口', kind: 'number', min: 0 }, input)).toThrow('范围');
    input.value = '65536'; expect(() => parseField({ key: 'port', label: '端口', kind: 'number', max: 65535 }, input)).toThrow('范围');
    input.value = '80'; expect(parseField({ key: 'port', label: '端口', kind: 'number' }, input)).toBe(80);
    input.value = 'invalid'; expect(() => parseField({ key: 'json', label: '配置', kind: 'json' }, input)).toThrow('JSON');
    input.value = '{}'; expect(parseField({ key: 'json', label: '配置', kind: 'json' }, input)).toEqual({});
    const textarea = el('textarea'); textarea.value = 'a, b\nc'; expect(parseField({ key: 'list', label: '域名', kind: 'list' }, textarea)).toEqual(['a', 'b', 'c']);
    input.value = '中'.repeat(64); expect(parseField({ key: 'name', label: '名称', maxLength: 64 }, input)).toBe(input.value);
    input.value = '😀'.repeat(64); expect(parseField({ key: 'name', label: '名称', maxLength: 64 }, input)).toBe(input.value);
    input.value = '😀'.repeat(65); expect(() => parseField({ key: 'name', label: '名称', maxLength: 64 }, input)).toThrow('64 个字符');
    input.value = ` ${'中'.repeat(64)} `; expect(parseField({ key: 'name', label: '名称', maxLength: 64 }, input)).toBe(input.value.trim());
  });
  it('submits once while busy, restores focus, and clears secret controls on close', async () => {
    const trigger = button('编辑', () => {}); document.body.append(trigger); trigger.focus(); let resolve!: () => void;
    const submit = vi.fn(() => new Promise<void>(done => { resolve = done; }));
    const dialog = new FormDialog('编辑', [{ key: 'name', label: '名称', value: '设备', required: true }, { key: 'secret', label: '密钥', kind: 'password', secret: true }], submit);
    dialog.form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); dialog.form.dispatchEvent(new Event('submit', { cancelable: true }));
    expect(submit).toHaveBeenCalledOnce(); expect(dialog.busy).toBe(true); dialog.close(); expect(dialog.element.isConnected).toBe(true);
    resolve(); await vi.waitFor(() => expect(dialog.busy).toBe(false)); dialog.close(); expect(document.activeElement).toBe(trigger); expect(dialog.element.isConnected).toBe(false);
  });
  it('blocks replay of uncertain writes and shows meaningful field errors', async () => {
    const dialog = new FormDialog('创建', [{ key: 'reason', label: '理由', required: true }], async () => { throw new ApiError(0, 'network', '结果未知', true); });
    dialog.form.dispatchEvent(new Event('submit', { cancelable: true })); await vi.waitFor(() => expect(dialog.error.textContent).toContain('请填写'));
    (dialog.form.querySelector('input') as HTMLInputElement).value = '发布'; dialog.form.dispatchEvent(new Event('submit', { cancelable: true }));
    await vi.waitFor(() => expect(dialog.error.textContent).toContain('结果未知')); expect(dialog.submit.disabled).toBe(true); dialog.close();
    const action = vi.fn().mockResolvedValue(undefined); const confirm = confirmation('停用', '确认停用设备', action); (confirm.form.querySelector('input') as HTMLInputElement).value = '维护';
    confirm.form.dispatchEvent(new Event('submit', { cancelable: true })); await vi.waitFor(() => expect(confirm.element.isConnected).toBe(false)); expect(action).toHaveBeenCalledWith('维护');
  });
});
