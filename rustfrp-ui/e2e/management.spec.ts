import { expect, test, type Page, type APIRequestContext } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { createConnection, createServer } from 'node:net';

const password = process.env.RUSTFRP_E2E_PASSWORD ?? 'native ui test password';
async function login(page: Page): Promise<void> {
  await page.goto('/'); await page.getByLabel('账号', { exact: true }).fill('preview'); await page.getByLabel('密码', { exact: true }).fill(password); await page.getByTestId('login-submit').click();
  await expect(page.getByTestId('page-title')).toHaveText('概览'); await expect(page.locator('.metric-card')).toHaveCount(6);
}
async function token(api: APIRequestContext, csrf: string, name: string) {
  const response = await api.post('/api/v1/enroll-tokens', { headers: { 'x-csrf-token': csrf }, data: { name, ttl_minutes: 5, reason: '原生 UI 浏览器 E2E' } }); expect(response.ok()).toBe(true); return await response.json();
}
async function stop(process: ChildProcess): Promise<void> {
  if (process.exitCode !== null || process.signalCode !== null) return;
  const done = new Promise<void>(resolve => process.once('exit', () => resolve())); process.kill('SIGINT');
  let timer: ReturnType<typeof setTimeout> | undefined;
  await Promise.race([done, new Promise<void>(resolve => { timer = setTimeout(resolve, 3000); })]); clearTimeout(timer);
  if (process.exitCode === null && process.signalCode === null) { process.kill('SIGTERM'); await done; }
}
async function save(page: Page, reason = 'UI 端到端维护验证'): Promise<void> {
  const dialog = page.locator('dialog'); await dialog.getByLabel('操作理由').fill(reason); await dialog.getByTestId('dialog-submit').click(); await expect(dialog).toHaveCount(0);
}
async function nav(page: Page, label: string): Promise<void> { await page.locator('.nav-main,.nav-foot').getByRole('button', { name: label, exact: true }).click(); await expect(page.getByTestId('page-title')).toHaveText(label); }
async function roundtrip(port: number): Promise<string> {
  return await new Promise((resolve, reject) => {
    const socket = createConnection({ host: '127.0.0.1', port }); const timer = setTimeout(() => { socket.destroy(); reject(new Error('tunnel timeout')); }, 5000);
    socket.once('connect', () => socket.write('native-ui-probe')); socket.once('data', bytes => { clearTimeout(timer); socket.end(); resolve(bytes.toString()); }); socket.once('error', error => { clearTimeout(timer); reject(error); });
  });
}

test('real agents, TCP forwarding, CRUD, visitors, import/export, credentials and audit', async ({ page, context }, testInfo) => {
  test.setTimeout(90000);
  const suffix = `${testInfo.project.name}-${Date.now()}`;
  const folder = await mkdtemp(join(tmpdir(), 'rustfrp-native-ui-e2e-')); const agents: ChildProcess[] = []; const clients: string[] = [];
  const echo = createServer(socket => socket.on('data', bytes => socket.write(bytes))); await new Promise<void>(resolve => echo.listen(0, '127.0.0.1', resolve));
  const address = echo.address(); if (!address || typeof address === 'string') throw new Error('missing echo port');
  let csrf = ''; const errors: string[] = []; page.on('pageerror', error => errors.push(error.message));
  try {
    await login(page); const session = await (await context.request.get('/api/v1/auth/me')).json(); csrf = session.csrf;
    const node = await (await context.request.get('/api/v1/node')).json();
    for (let index = 0; index < 2; index++) {
      const name = `UI-${suffix}-${index}`; const enrollment = await token(context.request, csrf, name);
      const agent = spawn(resolve('../target/release/rustfrp-agent'), ['--mode', 'client', '--controller', `127.0.0.1:${node.edge_port}`, '--enroll-token', enrollment.token, '--name', name, '--state-dir', join(folder, `agent-${index}`), '--local-control', join(folder, `agent-${index}`, 'control.json'), '--tls-pin', node.tls_fingerprint], { stdio: 'ignore' });
      agents.push(agent);
      let clientId = '';
      await expect.poll(async () => { const list = await (await context.request.get('/api/v1/clients')).json(); const client = list.find((client: { name: string; online: boolean; id: string }) => client.name === name && client.online); clientId = client?.id ?? ''; return !!client; }, { timeout: 15000 }).toBe(true);
      clients.push(clientId);
    }
    await nav(page, '节点管理'); await page.getByTestId('node-builtin').click(); await expect(page.getByRole('heading', { name: '网络与接入' })).toBeVisible(); await expect(page.getByText('已接入客户端', { exact: true }).last()).toBeVisible();
    await nav(page, '客户端'); await page.getByTestId(`detail-${clients[0]}`).click(); await page.getByTestId('edit-client').click();
    await page.getByLabel('名称', { exact: true }).fill(`已修改-${suffix}`); await page.getByLabel('标签', { exact: true }).fill('浏览器验证\n中文'); await page.getByLabel('全部隧道共享并发上限').fill('20'); await save(page);
    const changed = await (await context.request.get(`/api/v1/clients/${clients[0]}`)).json(); expect(changed.tags).toEqual(['浏览器验证', '中文']); expect(changed.max_connections).toBe(20);
    await page.getByTestId('edit-client').click(); const unicodeName = '😀'.repeat(64); const unicodeTag = '中文'.repeat(16);
    await page.getByLabel('名称', { exact: true }).fill(unicodeName); await page.getByLabel('标签', { exact: true }).fill(`${unicodeTag}\n${unicodeTag}`); await save(page);
    const unicodeClient = await (await context.request.get(`/api/v1/clients/${clients[0]}`)).json(); expect(unicodeClient.name).toBe(unicodeName); expect(unicodeClient.tags).toEqual([unicodeTag]);
    await page.getByTestId('client-new-tunnel').click(); await page.getByTestId('tunnel-template').selectOption('ssh');
    await page.getByLabel('名称', { exact: true }).fill(`TCP-${suffix}`); await page.getByLabel('本地端口', { exact: true }).fill(String(address.port)); await save(page);
    let tunnelId = ''; let remote = 0;
    await expect.poll(async () => { const list = await (await context.request.get(`/api/v1/tunnels?client_id=${clients[0]}`)).json(); const item = list.find((item: { name: string }) => item.name === `TCP-${suffix}`); tunnelId = item?.id ?? ''; remote = item?.status_port ?? 0; return item?.state; }, { timeout: 10000 }).toBe('running');
    expect(await roundtrip(remote)).toBe('native-ui-probe');
    await nav(page, '隧道管理'); await page.getByTestId('tunnel-client-filter').selectOption(clients[0]); await page.getByTestId(`edit-${tunnelId}`).click(); await page.getByLabel('并发连接 / 活跃 UDP 源上限').fill('5'); await page.getByLabel('附加 AES-256-GCM 加密').check(); await page.getByLabel('有界 LZ4 压缩').check(); await save(page);
    await expect.poll(async () => { const item = await (await context.request.get(`/api/v1/tunnels/${tunnelId}`)).json(); return [item.use_encryption, item.use_compression, item.max_connections]; }).toEqual([true, true, 5]);
    await expect.poll(async () => {
      const item = await (await context.request.get(`/api/v1/tunnels/${tunnelId}`)).json(); remote = item.status_port;
      if (item.state !== 'running' || !remote) return 'pending';
      try { return await roundtrip(remote); } catch { return 'reconciling'; }
    }, { timeout: 10000 }).toBe('native-ui-probe');
    await page.getByTestId(`select-${tunnelId}`).check(); await page.getByRole('button', { name: '批量停用', exact: true }).click(); await save(page);
    await expect.poll(async () => (await (await context.request.get(`/api/v1/tunnels/${tunnelId}`)).json()).enabled).toBe(false);
    await page.getByTestId(`toggle-${tunnelId}`).click(); await save(page);
    await nav(page, '客户端'); await page.getByTestId(`detail-${clients[1]}`).click(); await page.getByTestId('client-new-tunnel').click(); await page.getByLabel('类型', { exact: true }).selectOption('stcp');
    await page.getByLabel('名称', { exact: true }).fill(`私密-${suffix}`); await page.getByLabel('本地端口', { exact: true }).fill(String(address.port)); await save(page);
    await nav(page, '客户端'); await page.getByTestId(`detail-${clients[0]}`).click(); await page.getByTestId('client-new-visitor').click(); await page.getByLabel('名称', { exact: true }).fill(`访问者-${suffix}`); await page.getByLabel('附加加密', { exact: true }).check(); await page.getByLabel('压缩', { exact: true }).check(); await save(page);
    const visitors = await (await context.request.get(`/api/v1/visitors?client_id=${clients[0]}`)).json(); const visitor = visitors.find((item: { name: string }) => item.name === `访问者-${suffix}`); expect(visitor.use_encryption && visitor.use_compression).toBe(true);
    await page.getByTestId(`edit-visitor-${visitor.id}`).click(); await page.getByLabel('名称', { exact: true }).fill(`新访问者-${suffix}`); await save(page); await page.getByTestId(`toggle-visitor-${visitor.id}`).click(); await save(page); await page.getByTestId(`delete-visitor-${visitor.id}`).click(); await save(page);
    await page.getByTestId('import-config').click(); await page.getByLabel('TOML / YAML / JSON 完整配置').fill(`[[proxies]]\nname = "导入-${suffix}"\ntype = "tcp"\nlocalIP = "127.0.0.1"\nlocalPort = ${address.port}\nremotePort = 0`);
    await page.getByTestId('config-preview').click(); await expect(page.getByText('预览 1 条变更')).toBeVisible(); await page.getByLabel('已检查兼容报告，并确认按上述变更导入').check(); await save(page);
    await page.getByTestId('export-config').click(); await expect(page.locator('dialog textarea')).toHaveValue(new RegExp(`导入-${suffix}`)); await page.locator('dialog').getByRole('button', { name: '完成', exact: true }).click();
    await nav(page, '系统设置'); await page.getByTestId('setting-api').click(); await page.getByTestId('new-api-token').click(); await page.getByLabel('名称', { exact: true }).fill(`API-${suffix}`); await page.getByLabel('权限范围', { exact: true }).selectOption('read'); await page.getByLabel('操作理由').fill('API Token E2E'); await page.getByTestId('dialog-submit').click();
    await expect(page.locator('dialog h2')).toHaveText('保存 API Token'); const original = await page.locator('dialog textarea').inputValue(); expect(original).toMatch(/^rfrp_/); await page.locator('dialog').getByRole('button', { name: '完成', exact: true }).click(); expect(await page.locator('body').textContent()).not.toContain(original);
    const tokens = await (await context.request.get('/api/v1/api-tokens')).json(); const apiToken = tokens.find((item: { name: string }) => item.name === `API-${suffix}`); await page.getByTestId(`revoke-api-token-${apiToken.id}`).click(); await save(page);
    await nav(page, '审计日志'); await expect(page.getByText('UI 端到端维护验证', { exact: true }).first()).toBeVisible(); await expect(page.getByText('创建隧道', { exact: true }).first()).toBeVisible(); expect(errors).toEqual([]);
  } finally {
    for (const agent of agents) await stop(agent);
    for (const id of clients) await context.request.delete(`/api/v1/clients/${id}?reason=UI%20E2E%20cleanup`, { headers: { 'x-csrf-token': csrf } });
    await new Promise<void>(resolve => echo.close(() => resolve())); await rm(folder, { recursive: true, force: true });
  }
});
