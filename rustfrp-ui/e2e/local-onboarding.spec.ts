import { test, expect } from '@playwright/test';

// Exercise the shipped desktop DOM in each engine; native WebView tests cover IPC separately.
test('desktop connection forms stay compact and local action groups align', async ({ page }, info) => {
  await page.addInitScript(() => {
    let local: Record<string, unknown> = {};
    let running = false;
    Object.assign(window, { isTauri: true, __TAURI_INTERNALS__: { invoke: async (_command: string, { action, args }: { action: string; args: Record<string, unknown> }) => {
      if (action === 'preferences') return { local_mode: true, local };
      if (action === 'info') return { local_defaults: { binary: '/test/rustfrp-agent', stateDir: '/test/identity', name: '本机' } };
      if (action === 'savePreferences') { local = args.local as Record<string, unknown>; return {}; }
      if (action === 'localStatus') return running ? { connected: true, pid: 123, agent_version: 'test', revision: 1, proxies: 0 } : null;
      if (action === 'localStart') running = true;
      return null;
    } } });
  });
  await page.goto('/'); await page.evaluate(() => document.fonts.ready);
  await expect(page.getByTestId('local-start')).toHaveText('登录并连接此电脑');
  await page.getByTestId('local-settings').click();
  const dialog = page.locator('dialog');
  await expect(dialog.getByLabel('Agent 程序', { exact: true })).toBeHidden();
  await expect(dialog.getByLabel('证书 SHA-256 指纹')).toBeHidden();
  for (const [width, theme] of [[1440, 'light'], [390, 'light'], [390, 'dark'], [1440, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 }); await page.locator('html').evaluate((element, value) => { (element as HTMLElement).dataset.theme = value; }, theme);
    expect(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    await expect(page.getByTestId('dialog-submit')).toBeInViewport();
    await page.screenshot({ path: info.outputPath(`connection-${width}-${theme}.png`), animations: 'disabled' });
  }
  await dialog.getByLabel('接入地址', { exact: true }).fill('127.0.0.1:17000'); await page.getByTestId('dialog-submit').click();
  await expect(dialog).toHaveCount(0); await page.getByTestId('refresh-page').click();
  const align = async (ids: string[]) => {
    const boxes = await Promise.all(ids.map(id => page.getByTestId(id).boundingBox()));
    expect(boxes.every(box => box && Math.abs(box.y - boxes[0]!.y) < 1 && box.height === boxes[0]!.height)).toBe(true);
  };
  await align(['local-start', 'local-settings']);
  await expect(page.getByTestId('local-start')).toHaveText('登录并连接此电脑');
  await page.locator('summary', { hasText: '管理员手动接入' }).click();
  await page.getByTestId('local-manual-enroll').click(); await page.getByLabel('一次性接入令牌', { exact: true }).fill('test-only'); await page.getByTestId('dialog-submit').click();
  await page.getByTestId('refresh-page').click();
  await align(['local-tab-status', 'local-tab-tunnels', 'local-tab-logs']);
  await page.screenshot({ path: info.outputPath('local-buttons-aligned.png') });
});
