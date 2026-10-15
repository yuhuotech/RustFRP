import { expect, test, type Page } from '@playwright/test';

const password = process.env.RUSTFRP_E2E_PASSWORD ?? 'native ui test password';
async function login(page: Page, name: string, secret: string) {
  await page.goto('/');
  await page.getByLabel('账号', { exact: true }).fill(name);
  await page.getByLabel('密码', { exact: true }).fill(secret);
  await page.getByTestId('login-submit').click();
  await expect(page.getByTestId('page-title')).toHaveText('概览');
}

test('real roles restrict controls and writes, and protect the current owner', async ({ page, context, playwright }, info) => {
  test.setTimeout(60000);
  const owner = await playwright.request.newContext({ baseURL: 'http://127.0.0.1:9674', extraHTTPHeaders: { Origin: 'http://127.0.0.1:9674', 'x-rustfrp-admin': '1' } });
  const accounts: string[] = [];
  const reason = 'Native role E2E';
  let csrf = '';
  try {
    expect((await owner.post('/api/v1/auth/login', { data: { name: 'preview', password } })).ok()).toBe(true);
    const identity = await (await owner.get('/api/v1/auth/me')).json(); csrf = identity.csrf;
    const headers = { 'x-csrf-token': csrf, Origin: 'http://127.0.0.1:9674', 'x-rustfrp-admin': '1' };
    for (const role of ['viewer', 'operator', 'admin']) {
      const name = `ui-role-${info.project.name}-${role}-${Date.now()}`;
      const secret = 'native role test password';
      const response = await owner.post('/api/v1/admins', { headers, data: { name, password: secret, role, reason } });
      expect(response.ok()).toBe(true); const account = await response.json(); accounts.push(account.id);
      await context.clearCookies(); await login(page, name, secret);
      await page.locator('.nav-main').getByRole('button', { name: '客户端', exact: true }).click();
      await expect(page.getByTestId('page-title')).toHaveText('客户端');
      await expect(page.getByTestId('new-client')).toHaveCount(role === 'admin' ? 1 : 0);
      await page.locator('.nav-main').getByRole('button', { name: '节点管理', exact: true }).click();
      await expect(page.getByTestId('new-node')).toHaveCount(role === 'admin' ? 1 : 0);
      await page.locator('.nav-main').getByRole('button', { name: '隧道管理', exact: true }).click();
      await expect(page.getByTestId('new-tunnel')).toHaveCount(role === 'viewer' ? 0 : 1);
      await page.locator('.nav-foot').getByRole('button', { name: '系统设置', exact: true }).click();
      await expect(page.getByTestId('setting-admins')).toHaveCount(0);
      await expect(page.getByTestId('setting-api')).toHaveCount(0);
      await expect(page.getByTestId('setting-enroll')).toHaveCount(role === 'admin' ? 1 : 0);
      const session = await (await context.request.get('/api/v1/auth/me')).json();
      expect(session.admin.role).toBe(role);
      const forbidden = await context.request.post('/api/v1/admins', { headers: { ...headers, 'x-csrf-token': session.csrf }, data: { name: 'must-not-exist', password: secret, role: 'owner', reason } });
      expect(forbidden.status()).toBe(403);
      if (role !== 'admin') {
        const enrollment = await context.request.post('/api/v1/enroll-tokens', { headers: { ...headers, 'x-csrf-token': session.csrf }, data: { name: 'forbidden enrollment', ttl_minutes: 5, reason } });
        expect(enrollment.status()).toBe(403);
      }
    }
    await context.clearCookies(); await login(page, 'preview', password);
    await page.locator('.nav-foot').getByRole('button', { name: '系统设置', exact: true }).click();
    await page.getByTestId('setting-admins').click();
    await page.getByTestId(`edit-admin-${identity.admin.id}`).click();
    await expect(page.locator('dialog').getByLabel('角色', { exact: true })).toBeDisabled();
    await expect(page.locator('dialog').getByLabel('停用账号', { exact: true })).toBeDisabled();
    await page.keyboard.press('Escape');
    const refused = await owner.patch(`/api/v1/admins/${identity.admin.id}`, { headers, data: { disabled: true, reason } });
    expect(refused.status()).toBe(403);
    expect((await refused.json()).error).toMatchObject({ code: 'forbidden', message: 'cannot change your own role or status' });
    expect((await (await owner.get('/api/v1/auth/me')).json()).admin.id).toBe(identity.admin.id);
    // A cleared private session must return to login before displaying management data.
    await context.clearCookies(); await page.reload();
    await expect(page.getByTestId('login-submit')).toBeVisible();
    await expect(page.locator('.resource-table')).toHaveCount(0);
  } finally {
    for (const id of accounts) {
      const response = await owner.patch(`/api/v1/admins/${id}`, { headers: { 'x-csrf-token': csrf, Origin: 'http://127.0.0.1:9674', 'x-rustfrp-admin': '1' }, data: { disabled: true, reason: 'Native role E2E cleanup' } });
      expect(response.ok()).toBe(true);
    }
    await owner.dispose();
  }
});
