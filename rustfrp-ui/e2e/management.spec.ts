import { expect, test, type Page, type APIRequestContext } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
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
      const agent = spawn(resolve(import.meta.dirname, '../../target/release', process.platform === 'win32' ? 'rustfrp-agent.exe' : 'rustfrp-agent'), ['--mode', 'client', '--controller', `127.0.0.1:${node.edge_port}`, '--enroll-token', enrollment.token, '--name', name, '--state-dir', join(folder, `agent-${index}`), '--local-control', join(folder, `agent-${index}`, 'control.json'), '--tls-pin', node.tls_fingerprint], { stdio: 'ignore' });
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

test('independent node enrollment, management and removal use the real Controller and Agent', async ({ page, context }, testInfo) => {
  test.setTimeout(90000);
  const suffix = `${testInfo.project.name}-${Date.now()}`;
  const name = `独立节点-${suffix}`;
  const renamed = `已修改节点-${suffix}`;
  const folder = await mkdtemp(join(tmpdir(), 'rustfrp-node-ui-e2e-'));
  const errors: string[] = [];
  let csrf = '';
  let tokenId = '';
  let nodeId = '';
  let edge: ChildProcess | undefined;
  page.on('pageerror', error => errors.push(error.message));
  try {
    await login(page);
    csrf = (await (await context.request.get('/api/v1/auth/me')).json()).csrf;
    const builtin = await (await context.request.get('/api/v1/node')).json();
    await nav(page, '节点管理');
    const tokenResponse = page.waitForResponse(response => response.url().endsWith('/api/v1/enroll-tokens') && response.request().method() === 'POST');
    await page.getByTestId('new-node').click();
    await page.getByLabel('节点名称', { exact: true }).fill(name);
    await page.getByLabel('操作理由', { exact: true }).fill('独立节点接入 E2E');
    await page.getByTestId('dialog-submit').click();
    const enrollment = await (await tokenResponse).json();
    tokenId = enrollment.id;
    expect(enrollment.kind).toBe('node');
    expect(enrollment.commands.direct).toContain('--mode edge');
    expect(enrollment.commands.direct).toContain('--tls-pin');
    await expect(page.locator('dialog').getByLabel('direct', { exact: true })).toHaveValue(/--mode edge/);

    const config = join(folder, 'edge.toml');
    const edgePort = await new Promise<number>((resolve, reject) => {
      const listener = createServer();
      listener.once('error', reject);
      listener.listen(0, '127.0.0.1', () => {
        const address = listener.address();
        if (!address || typeof address === 'string') { listener.close(); reject(new Error('missing independent edge port')); return; }
        listener.close(error => error ? reject(error) : resolve(address.port));
      });
    });
    await writeFile(config, `bindAddr = "127.0.0.1"\nbindPort = ${edgePort}\nproxyBindAddr = "127.0.0.1"\n`);
    edge = spawn(resolve(import.meta.dirname, '../../target/release', process.platform === 'win32' ? 'rustfrp-agent.exe' : 'rustfrp-agent'), [
      '--mode', 'edge', '--controller', enrollment.commands.controller, '--enroll-token', enrollment.token,
      '--name', name, '--public-host', '127.0.0.1', '--state-dir', join(folder, 'node-state'),
      '--tls-pin', builtin.tls_fingerprint, '-c', config,
    ], { stdio: 'ignore' });

    let registered: { id: string; name: string; online: boolean; disabled_at: number | null; clients: number } | undefined;
    await expect.poll(async () => {
      const nodes = await (await context.request.get('/api/v1/nodes')).json();
      registered = nodes.find((item: typeof registered) => item?.name === name && item.online);
      return !!registered;
    }, { timeout: 20000 }).toBe(true);
    nodeId = registered!.id;
    await expect(page.getByRole('button', { name: '查看已接入资源', exact: true })).toBeEnabled({ timeout: 15000 });
    await page.getByRole('button', { name: '查看已接入资源', exact: true }).click();
    await expect(page.getByRole('heading', { name, exact: true })).toBeVisible();

    await page.getByTestId('rename-node').click();
    await page.getByLabel('名称', { exact: true }).fill(renamed);
    await save(page, '重命名独立节点 E2E');
    await expect.poll(async () => {
      const nodes = await (await context.request.get('/api/v1/nodes')).json();
      return nodes.find((item: { id: string }) => item.id === nodeId)?.name;
    }).toBe(renamed);

    await page.getByTestId('toggle-node').click();
    await save(page, '停用独立节点 E2E');
    await expect.poll(async () => {
      const nodes = await (await context.request.get('/api/v1/nodes')).json();
      return nodes.find((item: { id: string }) => item.id === nodeId)?.disabled_at != null;
    }).toBe(true);
    await stop(edge);
    await page.getByTestId('delete-node').click();
    await save(page, '删除独立节点 E2E');
    await expect.poll(async () => {
      const nodes = await (await context.request.get('/api/v1/nodes')).json();
      return nodes.some((item: { id: string }) => item.id === nodeId);
    }).toBe(false);
    nodeId = '';
    const actions = (await (await context.request.get('/api/v1/audit?limit=100')).json()).map((entry: { action: string }) => entry.action);
    expect(actions).toContain('node_token.create');
    expect(actions).toContain('node.enroll');
    expect(actions).toContain('node.update');
    expect(actions).toContain('node.delete');
    expect(errors).toEqual([]);
  } finally {
    if (edge) await stop(edge);
    if (!nodeId && csrf) {
      const nodes = await (await context.request.get('/api/v1/nodes')).json();
      nodeId = nodes.find((item: { id: string; name: string }) => item.name === name || item.name === renamed)?.id ?? '';
    }
    if (nodeId) {
      const headers = { 'x-csrf-token': csrf };
      await context.request.patch(`/api/v1/nodes/${nodeId}`, { headers, data: { disabled: true, reason: '独立节点 E2E 清理' } });
      await context.request.delete(`/api/v1/nodes/${nodeId}?reason=independent%20node%20E2E%20cleanup`, { headers });
    }
    if (tokenId) await context.request.delete(`/api/v1/enroll-tokens/${tokenId}?reason=independent%20node%20E2E%20cleanup`, { headers: { 'x-csrf-token': csrf } });
    await rm(folder, { recursive: true, force: true });
  }
});

test('all supported tunnel kinds save through the real Controller from the shared editor', async ({ page, context }, testInfo) => {
  test.setTimeout(90000);
  const suffix = `${testInfo.project.name}-${Date.now()}`;
  const name = `全部类型-${suffix}`;
  const folder = await mkdtemp(join(tmpdir(), 'rustfrp-tunnel-types-e2e-'));
  const tunnels: string[] = [];
  let csrf = '';
  let clientId = '';
  let agent: ChildProcess | undefined;
  try {
    await login(page);
    csrf = (await (await context.request.get('/api/v1/auth/me')).json()).csrf;
    const node = await (await context.request.get('/api/v1/node')).json();
    const enrollment = await token(context.request, csrf, name);
    agent = spawn(resolve(import.meta.dirname, '../../target/release', process.platform === 'win32' ? 'rustfrp-agent.exe' : 'rustfrp-agent'), [
      '--mode', 'client', '--controller', `127.0.0.1:${node.edge_port}`, '--enroll-token', enrollment.token,
      '--name', name, '--state-dir', join(folder, 'client-state'), '--tls-pin', node.tls_fingerprint,
    ], { stdio: 'ignore' });
    await expect.poll(async () => {
      const items = await (await context.request.get('/api/v1/clients')).json();
      const found = items.find((item: { name: string; online: boolean; id: string }) => item.name === name && item.online);
      clientId = found?.id ?? '';
      return !!found;
    }, { timeout: 15000 }).toBe(true);

    await nav(page, '客户端');
    await page.getByTestId(`detail-${clientId}`).click();
    for (const kind of ['tcp', 'udp', 'http', 'https', 'stcp', 'sudp', 'tcpmux']) {
      await page.getByTestId('client-new-tunnel').click();
      const dialog = page.locator('dialog');
      await dialog.getByLabel('类型', { exact: true }).selectOption(kind);
      const tunnelName = `${kind}-${suffix}`;
      await dialog.getByLabel('名称', { exact: true }).fill(tunnelName);
      await dialog.getByLabel('本地端口', { exact: true }).fill('8080');
      await dialog.getByLabel('启用隧道', { exact: true }).uncheck();
      if (['http', 'https', 'tcpmux'].includes(kind)) {
        await dialog.getByLabel('自定义域名', { exact: true }).fill(`${kind}-${suffix}.e2e.example.test`);
      }
      if (kind === 'http') {
        await dialog.getByLabel('改写 Host', { exact: true }).fill('backend.internal');
        await dialog.getByLabel('路径前缀', { exact: true }).fill('/api');
        await dialog.getByLabel('Basic Auth 用户名', { exact: true }).fill('e2e-user');
        await dialog.getByLabel('Basic Auth 密码', { exact: true }).fill('e2e-password');
        await dialog.getByLabel('按 Basic 用户名选路', { exact: true }).fill('e2e-route');
        await dialog.getByLabel('请求头', { exact: true }).fill('X-E2E-Request: request');
        await dialog.getByLabel('响应头', { exact: true }).fill('X-E2E-Response: response');
      }
      await dialog.getByLabel('操作理由', { exact: true }).fill(`验证 ${kind.toUpperCase()} 配置`);
      await page.getByTestId('dialog-submit').click();
      await expect(dialog).toHaveCount(0);
      let tunnel: { id: string; kind: string; enabled: boolean; custom_domains?: string[]; http_password_set?: boolean; http_password?: string; request_headers?: object; response_headers?: object } | undefined;
      await expect.poll(async () => {
        const values = await (await context.request.get(`/api/v1/tunnels?client_id=${clientId}`)).json();
        tunnel = values.find((item: { name: string }) => item.name === tunnelName);
        return tunnel?.kind;
      }).toBe(kind);
      expect(tunnel?.enabled).toBe(false);
      if (kind === 'http') {
        expect(tunnel?.custom_domains).toContain(`${kind}-${suffix}.e2e.example.test`);
        expect(tunnel?.http_password_set).toBe(true);
        expect(tunnel?.http_password).toBeUndefined();
        expect(tunnel?.request_headers).toEqual({ 'X-E2E-Request': 'request' });
        expect(tunnel?.response_headers).toEqual({ 'X-E2E-Response': 'response' });
      }
      if (tunnel) tunnels.push(tunnel.id);
    }
    await expect(page.getByTestId('page-title')).toHaveText('客户端');
  } finally {
    if (agent) await stop(agent);
    for (const id of tunnels) await context.request.delete(`/api/v1/tunnels/${id}?reason=tunnel%20kind%20E2E%20cleanup`, { headers: { 'x-csrf-token': csrf } });
    if (clientId) await context.request.delete(`/api/v1/clients/${clientId}?reason=tunnel%20kind%20E2E%20cleanup`, { headers: { 'x-csrf-token': csrf } });
    await rm(folder, { recursive: true, force: true });
  }
});
