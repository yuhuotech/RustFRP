import { describe, it, expect, vi } from 'vitest';
import { observeWindow } from '../src/platform/window';
import { WebHost, type Host } from '../src/platform/host';
import { FormDialog } from '../src/components/forms';

describe('native window release guard', () => {
  it('reports only flags, blocks credentials and dirty forms, and detaches on disposal', async () => {
    const call = vi.fn().mockResolvedValue(null);
    const host = { desktop: true, call, request: vi.fn() } as Host;
    const dispose = observeWindow(host);
    expect(call).toHaveBeenLastCalledWith('windowState', { busy: false, dirty: false, sensitive: false });
    const dialog = new FormDialog('修改配置', [{ key: 'config', label: '配置', value: 'private key never sent' }], async () => {});
    await vi.waitFor(() => expect(call).toHaveBeenLastCalledWith('windowState', { busy: false, dirty: false, sensitive: true }));
    const input = dialog.control('config')!; input.value = 'private secret'; input.dispatchEvent(new Event('input', { bubbles: true }));
    await vi.waitFor(() => expect(call).toHaveBeenLastCalledWith('windowState', { busy: false, dirty: true, sensitive: true }));
    dialog.setBusy(true); await vi.waitFor(() => expect(call.mock.lastCall?.[1]).toMatchObject({ busy: true }));
    expect(JSON.stringify(call.mock.calls)).not.toContain('private');
    dialog.setBusy(false); dialog.close(); await vi.waitFor(() => expect(call).toHaveBeenLastCalledWith('windowState', { busy: false, dirty: false, sensitive: false }));
    const password = document.createElement('input'); password.type = 'password'; password.value = 'login password'; document.body.append(password); password.dispatchEvent(new Event('input', { bubbles: true }));
    await vi.waitFor(() => expect(call.mock.lastCall?.[1]).toMatchObject({ sensitive: true }));
    const count = call.mock.calls.length; dispose(); password.remove(); await Promise.resolve(); expect(call).toHaveBeenCalledTimes(count);
  });
  it('retries flags after a host error and leaves browser-only pages independent', async () => {
    expect(() => observeWindow(new WebHost())()).not.toThrow();
    const call = vi.fn().mockRejectedValueOnce(new Error('host unavailable')).mockResolvedValue(null);
    const dispose = observeWindow({ desktop: true, call, request: vi.fn() } as Host);
    await Promise.resolve(); await Promise.resolve(); document.dispatchEvent(new Event('visibilitychange'));
    await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(2));
    document.dispatchEvent(new Event('visibilitychange')); await Promise.resolve(); expect(call).toHaveBeenCalledTimes(2); dispose();
  });
});
