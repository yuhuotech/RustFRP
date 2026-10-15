import { test, expect } from '@playwright/test';

const password = process.env.RUSTFRP_E2E_PASSWORD ?? 'native ui test password';
async function login(page: import('@playwright/test').Page) {
  await page.goto('/');
  await page.getByLabel('账号', { exact: true }).fill('preview');
  await page.getByLabel('密码', { exact: true }).fill(password);
  await page.getByTestId('login-submit').click();
  await expect(page.getByTestId('page-title')).toHaveText('概览');
  await expect(page.locator('.metric-card')).toHaveCount(6);
}
test('real Controller login, session, navigation and CSRF boundary', async ({ page, context }) => {
  const failures: string[] = []; page.on('pageerror', error => failures.push(error.message));
  await login(page);
  const cookies = await context.cookies(); expect(cookies.find(cookie => cookie.name === 'rfrp_session')?.httpOnly).toBe(true);
  expect(await page.evaluate(() => document.cookie)).not.toContain('rfrp_session');
  await page.getByRole('button', { name: '节点管理', exact: true }).first().click();
  await expect(page.getByTestId('page-title')).toHaveText('节点管理');
  await expect(page.locator('.resource-table')).toBeVisible();
  const forbidden = await context.request.patch('/api/v1/nodes/unknown', { data: { name: 'test', reason: '验证 CSRF' } });
  expect(forbidden.status()).toBe(403);
  await page.getByRole('button', { name: '客户端', exact: true }).first().click();
  await expect(page.locator('.empty-state')).toBeVisible();
  await page.route('**/api/v1/clients**', route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: { code: 'unavailable', message: 'temporary E2E failure' } }) }));
  const failedRead = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/clients' && response.request().method() === 'GET');
  await page.getByTestId('refresh-page').click();
  expect((await failedRead).status()).toBe(503);
  await expect(page.locator('.toast.danger')).toContainText('temporary E2E failure');
  await page.unroute('**/api/v1/clients**');
  const recoveredRead = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/clients' && response.request().method() === 'GET');
  await page.getByTestId('refresh-page').click();
  expect((await recoveredRead).ok()).toBe(true);
  await expect(page.locator('.empty-state')).toBeVisible();
  await page.reload(); await expect(page.getByTestId('page-title')).toHaveText('概览');
  await page.getByRole('button', { name: '系统设置', exact: true }).first().click();
  await page.getByRole('button', { name: '深色', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.getByRole('button', { name: '退出登录', exact: true }).click();
  await expect(page.getByTestId('login-submit')).toBeVisible(); expect(failures).toEqual([]);
});
test('narrow navigation and strict CSP render real controls without a canvas engine', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 780 }); await login(page);
  await expect(page.locator('.bottom-nav')).toBeVisible();
  expect(await page.locator('canvas').count()).toBe(0);
  await page.locator('.bottom-nav').getByRole('button', { name: '客户端', exact: true }).click();
  await expect(page.getByTestId('page-title')).toHaveText('客户端');
  await page.locator('.bottom-nav').getByRole('button', { name: '系统设置', exact: true }).click();
  await page.getByRole('button', { name: '深色', exact: true }).click();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth); expect(overflow).toBe(false);
});
test('system fonts, compact breakpoints and keyboard composition preserve an open form across polling', async ({ page }, info) => {
  await login(page); await page.evaluate(() => document.fonts.ready);
  for (const [width, columns] of [[1440, 6], [1024, 3], [720, 2], [390, 2], [300, 1]]) {
    await page.setViewportSize({ width, height: 900 });
    const count = await page.locator('.metrics').evaluate(element => getComputedStyle(element).gridTemplateColumns.split(' ').length); expect(count).toBe(columns);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    await page.screenshot({ path: info.outputPath(`overview-${width}-light.png`), fullPage: true });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.locator('.nav-foot').getByRole('button', { name: '系统设置', exact: true }).click(); await page.getByRole('button', { name: '深色', exact: true }).click(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.locator('.nav-main').getByRole('button', { name: '概览', exact: true }).click(); await expect(page.locator('.metric-card')).toHaveCount(6); await page.screenshot({ path: info.outputPath('overview-1440-dark.png'), fullPage: true });
  const fonts = await page.locator('body').evaluate(element => getComputedStyle(element).fontFamily); expect(fonts).toMatch(/system|apple|Segoe/); expect(await page.locator('canvas').count()).toBe(0);
  await page.evaluate(() => { document.documentElement.style.zoom = '125%'; });
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await page.evaluate(() => { document.documentElement.style.zoom = ''; });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  expect(await page.locator('.metric-card').first().evaluate(element => getComputedStyle(element).transitionDuration)).toBe('0s');
  await page.locator('.nav-main').getByRole('button', { name: '客户端', exact: true }).click(); await page.getByTestId('new-client').click(); const field = page.locator('dialog').getByLabel('客户端名称', { exact: true });
  await field.dispatchEvent('compositionstart', { data: 'zhong' }); await field.fill('中文输入测试 🦀'); await field.dispatchEvent('compositionupdate', { data: '中文输入测试' }); await field.dispatchEvent('compositionend', { data: '中文输入测试' });
  // Wait in the test runner, not the renderer, so a throttled Chromium page timer cannot consume the test timeout.
  await new Promise(resolve => setTimeout(resolve, 5100)); await expect(field).toHaveValue('中文输入测试 🦀'); await expect(field).toBeFocused(); await page.keyboard.press('Tab'); await expect(page.locator('dialog input[name=ttl_minutes]')).toBeFocused(); await page.keyboard.press('Escape'); await expect(page.locator('dialog')).toHaveCount(0); await expect(page.getByTestId('new-client')).toBeFocused();
});
