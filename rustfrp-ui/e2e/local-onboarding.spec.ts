import { test, expect } from '@playwright/test';
test('desktop ignores legacy local mode and presents only server and management token', async ({ page }, info) => {
  await page.addInitScript(() => {
    Object.assign(window, { isTauri: true, __TAURI_INTERNALS__: { invoke: async (command: string, { action }: { action: string }) => {
      if (command === 'controller_request') throw { status: 401, message: '请填写管理 token' };
      if (action === 'preferences') return { local_mode: true, local: { controller: '127.0.0.1:7000' } };
      return null;
    } } });
  });
  await page.goto('/'); await expect(page.getByTestId('login-submit')).toBeVisible();
  await expect(page.getByLabel('管理服务器', { exact: true })).toHaveValue('http://127.0.0.1:9674');
  await expect(page.locator('#login-name,#login-password,[data-testid=local-start],[data-testid=login-local]')).toHaveCount(0);
  for (const [width, theme] of [[1440, 'light'], [390, 'light'], [390, 'dark'], [1440, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 }); await page.locator('html').evaluate((element, value) => { (element as HTMLElement).dataset.theme = value; }, theme);
    await expect(page.getByTestId('login-submit')).toBeInViewport();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath('token-login-' + width + '-' + theme + '.png'), animations: 'disabled' });
  }
});
