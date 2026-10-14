import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiClient, type Operation, type RequestArgs } from '../src/api/client';
import type { Admin, Client, Json, NodeInfo, RecordValue, Tunnel, Visitor, Role } from '../src/api/models';
import { Resources } from '../src/app/resources';
import type { Context, Page } from '../src/app/context';
import { WebHost } from '../src/platform/host';
import { nodesPage, nodeCapabilities, nodePorts } from '../src/features/nodes';
import { clientsPage } from '../src/features/clients';
import { tunnelEditor, tunnelsPage } from '../src/features/tunnels';
import { visitorEditor, visitorTargets } from '../src/features/visitors';
import { configImport, configExport } from '../src/features/config-transfer';
import { enroll, showEnrollment } from '../src/features/enrollment';
import { settingsPage, settingSections } from '../src/features/settings';
import { auditPage, actionLabel } from '../src/features/audit';
import { LogView } from '../src/components/logs';
import { headers, tunnelBody, tunnelFields, tunnelKinds } from '../src/features/tunnel-spec';
import { parseField } from '../src/components/forms';
import { secretDialog } from '../src/components/operations';
import { overviewPage } from '../src/features/overview';

const client = (id = 'c', extra: RecordValue = {}): Client => ({ id, name: `设备 ${id}`, online: true, node_id: '', os: 'linux', arch: 'x86_64', version: '0.1.0', tags: ['中文'], last_ip: '203.0.113.4', disabled_at: null, max_connections: 0, revision: 1, last_seen_at: 1700000000000, created_at: 1700000000000, ...extra });
const node = (id = '', extra: RecordValue = {}): NodeInfo => ({ id, name: id ? '独立节点' : 'builtin', builtin: !id, online: true, disabled_at: null, public_host: 'example.com', edge_port: 7000, tls_fingerprint: 'test-pin', version: '0.1.0', clients: 2, allow_ports: [[5000, 6000]], vhost_http: 80, vhost_https: 443, tcpmux_http_connect_port: 5002, subdomain_host: 'example.com', ...extra });
const tunnel = (id = 't', extra: RecordValue = {}): Tunnel => ({ id, name: `隧道 ${id}`, kind: 'tcp', client_id: 'c', enabled: true, state: 'running', status_error: '', status_port: 5001, local_ip: '127.0.0.1', local_port: 80, remote_port: 0, custom_domains: [], subdomain: '', ...extra });
const visitor = (extra: RecordValue = {}): Visitor => ({ id: 'v', name: '访问者', client_id: 'c', tunnel_id: 'private', bind_addr: '127.0.0.1', bind_port: 3000, enabled: true, use_encryption: true, use_compression: true, ...extra });
const live: { page?: Page; context: Context }[] = [];
afterEach(() => {
  for (const dialog of document.querySelectorAll<HTMLDialogElement>('dialog')) dialog.close();
  for (const entry of live.splice(0)) { entry.page?.dispose(); entry.context.resources.dispose(); entry.context.api.close(); }
});
function harness(role: Role = 'owner') {
  const data: Record<string, Json> = {
    nodes: [node(), node('edge')], clients: [client(), client('other'), client('far', { node_id: 'edge' })],
    tunnels: [tunnel(), tunnel('private', { name: '私密', kind: 'stcp', client_id: 'other' })], visitors: [visitor()],
    admins: [{ id: 'a', name: '当前用户', role: 'owner', disabled_at: null, created_at: 1 }, { id: 'b', name: '其他管理员', role: 'admin', disabled_at: null, created_at: 2 }],
    'enroll-tokens': [{ id: 'tok', name: '测试接入', kind: 'client', expires_at: Date.now() + 60000, used_at: null, revoked_at: null, created_at: 1, created_by: '当前用户' }],
    'api-tokens': [{ id: 'api', name: '自动化', scope: 'tunnels', expires_at: Date.now() + 60000, revoked_at: null }],
    audit: [{ id: 12, ts: 1700000000000, actor: '当前用户', action: 'client.update', target_kind: 'client', target_id: 'c', reason: '维护', result: 'ok', ip: '127.0.0.1' }],
    logs: { items: ['controller ready'], cursor: 1, gap: false, capacity: 128 },
    overview: { counts: { tunnels_running: 1, tokens_pending: 1 }, clients_online: 3, traffic: { connections: 0 }, traffic_24h: { bytes_in: 10, bytes_out: 20 }, process: { rss_bytes: 1024 }, version: '0.1.0' },
    traffic: { range_hours: 24, bucket_ms: 3600000, window_end: 3600000, items: [] },
  };
  const reads: string[] = [];
  const fetcher = vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), 'http://localhost'); const path = url.pathname.split('/').at(-1)!; reads.push(url.pathname + url.search);
    let result = data[path] ?? {};
    if (path === 'preview') result = { digest: 'test-digest', changes: [{ name: '新增隧道', action: 'create', kind: 'tcp', local_ip: '127.0.0.1', local_port: 22, remote_port: 0, enabled: true }], warnings: [] };
    if (path === 'export') result = { text: 'serverAddr = "example.com"\nhttpPassword = "test-only"' };
    return new Response(JSON.stringify(result));
  });
  const api = new ApiClient(undefined, fetcher); const resources = new Resources(api);
  const writes: { operation: Operation; args: RequestArgs }[] = [];
  const host = new WebHost(); host.call = vi.fn().mockResolvedValue({ version: '0.1.0' });
  const context: Context = {
    api, resources, host, admin: { id: 'a', name: '当前用户', role, disabled_at: null } as Admin,
    navigate: vi.fn(), refresh: vi.fn().mockResolvedValue(undefined), logout: vi.fn().mockResolvedValue(undefined), showError: vi.fn(),
    write: vi.fn(async (operation: Operation, args: RequestArgs = {}): Promise<Json> => {
      writes.push({ operation, args });
      if (operation === 'createEnrollToken') return { id: 'tok', token: 'test-once', expires_at: Date.now() + 60000, commands: { direct: 'test command', linux_macos: 'test install' } };
      if (operation === 'createApiToken') return { token: 'test-api-once', info: { id: 'api-new' } };
      return { ok: true };
    }),
  };
  live.push({ context }); return { context, data, fetcher, writes, reads };
}
async function mount(page: Page, context: Context): Promise<void> {
  live.find(item => item.context === context)!.page = page;
  document.getElementById('app')!.replaceChildren(...page.actions, page.element); await page.refresh();
}
function click(id: string): void { const control = document.querySelector<HTMLButtonElement>(`[data-testid="${id}"]`); expect(control, id).not.toBeNull(); control!.click(); }
function field(key: string, value: string | boolean): void {
  const input = document.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(`dialog [name="${key}"]`)!;
  expect(input, key).not.toBeNull();
  if (typeof value === 'boolean' && input instanceof HTMLInputElement) input.checked = value; else input.value = String(value);
  input.dispatchEvent(new Event('input', { bubbles: true })); input.dispatchEvent(new Event('change', { bubbles: true }));
}
function submit(): void { document.querySelector('dialog form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); }
async function reasonAndSave(writes: unknown[], expected = 1): Promise<void> { field('reason', '维护验证'); submit(); await vi.waitFor(() => expect(writes).toHaveLength(expected)); }

describe('nodes and client operations retain all controls', () => {
  it('opens the empty built-in ID as a detail and labels client counts accurately', async () => {
    const h = harness(); const page = nodesPage(h.context, ''); await mount(page, h.context);
    expect(page.element.textContent).toContain('内置服务端'); expect(page.element.textContent).toContain('已接入客户端');
    expect(page.element.textContent).toContain('网络与接入'); expect(page.element.textContent).toContain('证书 SHA-256');
    expect(document.querySelector('[data-testid=rename-node]')).toBeNull();
    expect(nodeCapabilities(node())).toContain('TCPMUX 5002'); expect(nodePorts(node())).toBe('5000–6000'); expect(nodePorts(node('', { allow_ports: [[7000, 7000]] }))).toBe('7000'); expect(nodePorts(node('', { allow_ports: [] }))).toBe('不限');
  });
  it('renames and toggles a node with audit reasons and enforces deletion preconditions', async () => {
    const h = harness(); const page = nodesPage(h.context, 'edge'); await mount(page, h.context);
    expect(document.querySelector<HTMLButtonElement>('[data-testid=delete-node]')!.disabled).toBe(true);
    click('rename-node'); field('name', '上海节点'); await reasonAndSave(h.writes); expect(h.writes[0]).toMatchObject({ operation: 'patchNode', args: { id: 'edge', body: { name: '上海节点', reason: '维护验证' } } });
    await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); click('toggle-node'); await reasonAndSave(h.writes, 2); expect(h.writes[1].args.body?.disabled).toBe(true);
    h.data.nodes = [node(), node('edge', { disabled_at: 1, clients: 0 })]; await page.refresh(true);
    await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); click('delete-node'); await reasonAndSave(h.writes, 3); expect(h.writes[2]).toMatchObject({ operation: 'deleteNode', args: { query: { reason: '维护验证' } } });
    expect(h.context.navigate).toHaveBeenCalledWith('nodes');
  });
  it('shows client groups, tunnel and visitor actions, searches without changing DOM identities', async () => {
    const h = harness(); const listing = clientsPage(h.context); await mount(listing, h.context);
    const row = listing.element.querySelector('[data-row-id=c]'); await listing.refresh(true); expect(listing.element.querySelector('[data-row-id=c]')).toBe(row);
    const search = listing.element.querySelector<HTMLInputElement>('input[type=search]')!; search.value = 'far'; search.dispatchEvent(new Event('input')); expect(listing.element.querySelectorAll('[data-row-id]')).toHaveLength(1);
    listing.dispose(); const page = clientsPage(h.context, 'c'); await mount(page, h.context); expect(page.element.textContent).toContain('设备信息'); expect(page.element.textContent).toContain('连接与接入');
    expect(document.querySelector('[data-testid=client-new-visitor]')).not.toBeNull(); expect(document.querySelector('[data-testid=client-new-tunnel]')).not.toBeNull();
    click('edit-client'); field('name', '新 NAS'); field('tags', '生产\n主机'); field('max_connections', '12'); await reasonAndSave(h.writes);
    expect(h.writes[0]).toMatchObject({ operation: 'patchClient', args: { body: { name: '新 NAS', tags: ['生产', '主机'], max_connections: 12 } } });
  });
  it('keeps viewer and operator permissions separate and disables unavailable online commands', async () => {
    const viewer = harness('viewer'); await mount(clientsPage(viewer.context, 'c'), viewer.context); expect(document.querySelector('[data-testid=edit-client]')).toBeNull(); expect(document.querySelector('[data-testid=import-config]')).toBeNull();
    const operator = harness('operator'); await mount(clientsPage(operator.context, 'c'), operator.context); expect(document.querySelector('[data-testid=import-config]')).not.toBeNull(); expect(document.querySelector('[data-testid=export-config]')).toBeNull();
    const admin = harness(); admin.data.clients = [client('c', { online: false })]; await mount(clientsPage(admin.context, 'c'), admin.context);
    for (const key of ['restart-client', 'update-client', 'kick-client', 'migrate-client']) expect(document.querySelector<HTMLButtonElement>(`[data-testid=${key}]`)!.disabled).toBe(true);
  });
  it('sends client disable, kick, restart, migration, update and delete with a reason', async () => {
    const h = harness(); await mount(clientsPage(h.context, 'c'), h.context);
    for (const [control, operation] of [['toggle-client', 'patchClient'], ['kick-client', 'kickClient'], ['restart-client', 'restartClient'], ['migrate-client', 'migrateClient'], ['update-client', 'updateClient'], ['delete-client', 'deleteClient']] as const) {
      const size = h.writes.length; click(control); if (control === 'update-client') field('version', 'v1.2.3'); await reasonAndSave(h.writes, size + 1);
      expect(h.writes.at(-1)?.operation).toBe(operation); expect(h.writes.at(-1)?.args.id).toBe('c'); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull());
    }
    expect(h.writes.find(write => write.operation === 'migrateClient')?.args.body?.node_id).toBe('edge'); expect(h.context.navigate).toHaveBeenCalledWith('clients');
  });
});

describe('complete tunnel fields and batch behavior', () => {
  it('retains write-only passwords and group keys, converts headers and all health settings', () => {
    const existing = tunnel('http', { kind: 'http', http_password_set: true, lb_group_key_set: true, custom_domains: ['example.com'], health_check: { kind: 'http', path: '/ready', timeout_secs: 2, max_failed: 3, interval_secs: 15, headers: { Host: 'example.com' } } });
    const values: RecordValue = {};
    for (const spec of tunnelFields([['c', '设备']], existing)) {
      const input = document.createElement(spec.kind === 'textarea' || spec.kind === 'json' || spec.kind === 'list' ? 'textarea' : 'input');
      input.value = spec.secret ? '' : spec.kind === 'json' ? '' : Array.isArray(spec.value) ? spec.value.join('\n') : String(spec.value ?? '');
      if (spec.kind === 'checkbox' && input instanceof HTMLInputElement) input.checked = spec.value === true;
      const parsed = parseField(spec, input); if (parsed !== undefined) values[spec.key] = parsed;
    }
    values.reason = '测试'; values.lb_group = 'group'; values.request_headers = 'X-Test: hello\nHost: backend'; values.response_headers = 'X-Reply: yes';
    const body = tunnelBody(values, existing); expect(body).not.toHaveProperty('http_password'); expect(body).not.toHaveProperty('lb_group_key'); expect(body).not.toHaveProperty('kind'); expect(body).not.toHaveProperty('client_id');
    expect(body.request_headers).toEqual({ 'X-Test': 'hello', Host: 'backend' }); expect(body.health_check).toMatchObject({ kind: 'http', path: '/ready', headers: { Host: 'example.com' } });
    values.clear_http_password = true; values.lb_group = ''; values.hc_kind = ''; expect(tunnelBody(values, existing)).toMatchObject({ http_password: '', lb_group_key: '', health_check: { kind: '' } });
    expect(() => headers('missing colon')).toThrow(); expect(() => headers('bad name: x')).toThrow(); expect(headers('\nX-Test: x')).toEqual({ 'X-Test': 'x' });
  });
  it('validates domain, location, health and plugin compatibility without silently dropping fields', () => {
    const base: RecordValue = { name: '测试', kind: 'http', client_id: 'c', local_ip: '127.0.0.1', local_port: 80, remote_port: 0, enabled: true, max_connections: 2, use_encryption: false, use_compression: false, proxy_protocol: '2', bandwidth_bps: 2048, bandwidth_server: true, custom_domains: [], subdomain: '', locations: [], http_user: '', request_headers: '', response_headers: '', host_header_rewrite: '', route_by_http_user: '', lb_group: '', hc_kind: '', plugin_action: 'keep', reason: 'test' };
    expect(() => tunnelBody(base)).toThrow('域名'); base.custom_domains = ['example.com']; base.locations = ['bad']; expect(() => tunnelBody(base)).toThrow('路径前缀');
    base.locations = ['/']; base.hc_kind = 'http'; base.hc_path = 'bad'; expect(() => tunnelBody(base)).toThrow('健康检查');
    base.hc_path = '/'; base.hc_headers = ''; base.hc_timeout = 3; base.hc_failed = 1; base.hc_interval = 10; base.plugin_action = 'replace'; base.plugin_json = { kind: 'unknown' }; expect(() => tunnelBody(base)).toThrow('插件类型');
    base.plugin_json = { kind: 'static_file', local_path: '/tmp/html' }; const body = tunnelBody(base); expect(body.plugin).toEqual(base.plugin_json); expect(body.proxy_protocol).toBe(2); expect(body.bandwidth_bps).toBe(2048);
    base.kind = 'udp'; expect(() => tunnelBody(base)).toThrow('插件只能'); base.plugin_action = 'remove'; expect(tunnelBody(base)).toMatchObject({ plugin: { kind: '' }, remote_port: 0 }); expect(tunnelBody(base)).not.toHaveProperty('bandwidth_bps');
    base.kind = 'tcpmux'; expect(tunnelBody(base).multiplexer).toBe('httpconnect'); base.kind = 'stcp'; expect(tunnelBody(base).remote_port).toBe(0); base.kind = 'xtcp'; expect(() => tunnelBody(base)).toThrow('不支持');
    expect(tunnelKinds).toContain('sudp');
  });
  it('applies a template and submits an immutable existing type with secret-preserving semantics', async () => {
    const h = harness(); await tunnelEditor(h.context); const picker = document.querySelector<HTMLSelectElement>('[data-testid=tunnel-template]')!; picker.value = 'ssh'; picker.dispatchEvent(new Event('change'));
    expect(document.querySelector<HTMLInputElement>('[name=name]')!.value).toBe('SSH'); expect(document.querySelector<HTMLInputElement>('[name=local_port]')!.value).toBe('22');
    await reasonAndSave(h.writes); expect(h.writes[0]).toMatchObject({ operation: 'createTunnel', args: { body: { kind: 'tcp', name: 'SSH', local_port: 22 } } }); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull());
    await tunnelEditor(h.context, tunnel('http', { kind: 'http', custom_domains: ['example.com'], http_password_set: true })); expect(document.querySelector<HTMLSelectElement>('[name=kind]')!.disabled).toBe(true);
    await reasonAndSave(h.writes, 2); expect(h.writes[1].args.body).not.toHaveProperty('http_password'); expect(h.writes[1].args.body).not.toHaveProperty('client_id');
  });
  it('preserves selections across filtering, caps batches at 100 and submits exact IDs', async () => {
    const h = harness(); h.data.tunnels = Array.from({ length: 150 }, (_, index) => tunnel(`t${index}`, { enabled: index % 2 === 0 })); const page = tunnelsPage(h.context); await mount(page, h.context);
    [...page.element.querySelectorAll('button')].find(button => button.textContent === '选择当前筛选')!.click(); expect(page.element.textContent).toContain('已选择 100 条');
    const check = page.element.querySelector<HTMLInputElement>('[data-testid=select-t149]')!; check.checked = true; check.dispatchEvent(new Event('change')); expect(document.querySelector('.toast')?.textContent).toContain('100');
    const filter = page.element.querySelector<HTMLSelectElement>('[data-testid=tunnel-state-filter]')!; filter.value = 'disabled'; filter.dispatchEvent(new Event('change')); expect(page.element.textContent).toContain('已选择 100 条');
    [...page.element.querySelectorAll('button')].find(button => button.textContent === '批量停用')!.click(); await reasonAndSave(h.writes); expect(h.writes[0].operation).toBe('batchTunnels'); expect(h.writes[0].args.body?.ids).toHaveLength(100); expect(h.writes[0].args.body?.enabled).toBe(false);
    await vi.waitFor(() => expect(page.element.textContent).toContain('已选择 0 条'));
  });
});

describe('visitors and configuration transfer', () => {
  it('only offers private tunnels on another client on the same node', async () => {
    const h = harness(); const clients = h.data.clients as Client[]; const tunnels = [tunnel('self', { kind: 'sudp' }), tunnel('private', { kind: 'stcp', client_id: 'other' }), tunnel('far', { kind: 'sudp', client_id: 'far' })];
    expect(visitorTargets('c', clients, tunnels).map(tunnel => tunnel.id)).toEqual(['private']); expect(visitorTargets('missing', clients, tunnels)).toEqual([]);
    await visitorEditor(h.context, 'c'); field('name', '桌面访问'); field('use_encryption', true); field('use_compression', true); await reasonAndSave(h.writes);
    expect(h.writes[0]).toMatchObject({ operation: 'createVisitor', args: { body: { client_id: 'c', tunnel_id: 'private', use_encryption: true, use_compression: true } } });
  });
  it('edits, toggles and deletes existing visitors without losing transformations', async () => {
    const h = harness(); await mount(clientsPage(h.context, 'c'), h.context); click('edit-visitor-v'); await vi.waitFor(() => expect(document.querySelector('dialog')).not.toBeNull());
    field('bind_port', '3001'); await reasonAndSave(h.writes); expect(h.writes[0]).toMatchObject({ operation: 'patchVisitor', args: { id: 'v', body: { bind_port: 3001, use_encryption: true, use_compression: true } } });
    await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); click('toggle-visitor-v'); await reasonAndSave(h.writes, 2); expect(h.writes[1].args.body?.enabled).toBe(false);
    await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); click('delete-visitor-v'); await reasonAndSave(h.writes, 3); expect(h.writes[2].operation).toBe('deleteVisitor');
  });
  it('requires a current digest and compatibility acknowledgment, clears preview on edited source', async () => {
    const h = harness(); const dialog = configImport(h.context, 'c'); field('text', '[[proxies]]\nname="ssh"\ntype="tcp"\nlocalPort=22'); expect(dialog.submit.disabled).toBe(true);
    click('config-preview'); await vi.waitFor(() => expect(dialog.content.textContent).toContain('预览 1 条变更')); expect(dialog.submit.disabled).toBe(false);
    field('text', 'changed'); expect(dialog.submit.disabled).toBe(true); expect(dialog.content.textContent).toContain('重新预览');
    click('config-preview'); await vi.waitFor(() => expect(dialog.submit.disabled).toBe(false)); await reasonAndSave(h.writes);
    expect(h.writes[0]).toMatchObject({ operation: 'importConfig', args: { id: 'c', body: { text: 'changed', digest: 'test-digest', accept_warnings: true, reason: '维护验证' } } });
  });
  it('shows export and credentials once, clears their controls and never puts originals in lists', async () => {
    const h = harness(); await configExport(h.context, 'c'); const field = document.querySelector<HTMLTextAreaElement>('dialog textarea')!; expect(field.value).toContain('httpPassword'); document.querySelector<HTMLDialogElement>('dialog')!.close(); expect(field.value).toBe('');
    const source = { 原文: 'test secret' }; const once = secretDialog(h.context, '保存', source, '仅一次'); const control = once.content.querySelector('textarea')!; once.close(); expect(control.value).toBe(''); expect(source.原文).toBe('');
  });
});

describe('settings, enrollment, logs and audit', () => {
  it('desktop account navigation, themes, startup and notifications use typed capabilities and roll back failed controls', async () => {
    const h = harness(); Object.defineProperty(h.context.host, 'desktop', { value: true });
    const call = vi.mocked(h.context.host.call); const prefs: RecordValue = { close_to_tray: false, lightweight: false, notifications: false };
    call.mockImplementation(async action => action === 'preferences' ? prefs : action === 'startup' ? false : action === 'updateStatus' ? { disabled: 'fixture key missing' } : { version: '0.1.0' });
    await mount(settingsPage(h.context), h.context); click('switch-controller'); expect(h.context.logout).toHaveBeenCalled();
    [...document.querySelectorAll('button')].find(control => control.textContent === '管理本机')!.click(); expect(h.context.navigate).toHaveBeenCalledWith('local'); click('setting-appearance'); expect(h.context.navigate).toHaveBeenCalledWith('settings', 'appearance');
    await mount(settingsPage(h.context, 'appearance'), h.context); for (const label of ['浅色', '深色', '跟随系统']) [...document.querySelectorAll('button')].find(control => control.textContent === label)!.click(); expect(call).toHaveBeenCalledWith('savePreferences', { theme: 'system' }); expect(document.documentElement.dataset.theme).toBe('light');
    await mount(settingsPage(h.context, 'desktop'), h.context);
    for (const label of ['关闭窗口保留托盘', '隐藏后释放窗口', '设备状态通知']) { const check = document.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!; expect(check.checked).toBe(false); check.checked = true; check.dispatchEvent(new Event('change')); await vi.waitFor(() => expect(check.disabled).toBe(false)); }
    expect(call).toHaveBeenCalledWith('setNotifications', { enabled: true }); expect(call).toHaveBeenCalledWith('savePreferences', { lightweight: true }); click('desktop-startup'); await vi.waitFor(() => expect(call).toHaveBeenCalledWith('setStartup', { enabled: true }));
    call.mockRejectedValue(new Error('native unavailable')); const check = document.querySelector<HTMLInputElement>('[aria-label="设备状态通知"]')!; check.checked = true; check.dispatchEvent(new Event('change')); await vi.waitFor(() => expect(check.disabled).toBe(false)); expect(check.checked).toBe(false); expect(h.context.showError).toHaveBeenCalled();
  });
  it('about pages retain safe external actions and native update status while web hides system controls', async () => {
    const h = harness(); await mount(settingsPage(h.context, 'about'), h.context); for (const label of ['官网', 'Release']) [...document.querySelectorAll('button')].find(control => control.textContent === label)!.click(); expect(h.context.host.call).toHaveBeenCalledWith('openUrl', expect.objectContaining({ url: 'https://rustfrp.com' }));
    Object.defineProperty(h.context.host, 'desktop', { value: true }); vi.mocked(h.context.host.call).mockImplementation(async (action): Promise<Json> => action === 'preferences' ? {} : action === 'updateStatus' ? { disabled: 'missing fixture key' } : { version: '0.1.0' }); const page = settingsPage(h.context, 'about'); await mount(page, h.context); await page.refresh(true); expect(document.body.textContent).toContain('missing fixture key'); expect(settingSections(h.context).some(([id]) => id === 'desktop')).toBe(true);
    h.context.admin = null; await mount(settingsPage(h.context), h.context); expect(document.body.textContent).toContain('未登录'); expect(document.body.textContent).toContain('本机模式');
  });
  it('creates admins, refuses empty protected-owner edits and renders expired and revoked credentials without raw tokens', async () => {
    const h = harness(); await mount(settingsPage(h.context, 'admins'), h.context); click('new-admin'); field('name', '审核员'); field('password', 'fixture password'); field('role', 'viewer'); await reasonAndSave(h.writes); expect(h.writes[0]).toMatchObject({ operation: 'createAdmin', args: { body: { name: '审核员', role: 'viewer' } } }); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull());
    click('edit-admin-a'); field('reason', '检查'); submit(); await vi.waitFor(() => expect(document.querySelector('.form-error')!.textContent).toContain('填写要修改')); expect(h.writes).toHaveLength(1); document.querySelector<HTMLDialogElement>('dialog')!.close();
    h.data['api-tokens'] = [{ id: 'expired', name: '过去', scope: 'future.scope', expires_at: 1, revoked_at: null }, { id: 'revoked', name: '已撤销', scope: 'admin', expires_at: 1, revoked_at: 2 }]; await mount(settingsPage(h.context, 'api'), h.context); expect(document.body.textContent).toContain('future.scope'); expect(document.body.textContent).toContain('已过期'); expect(document.querySelector('[data-testid=revoke-api-token-revoked]')).toBeNull();
    h.data['enroll-tokens'] = [h.data['enroll-tokens'] as Json].flatMap(value => Array.isArray(value) ? value : []); await mount(settingsPage(h.context, 'enroll'), h.context); click('revoke-enroll-tok'); await reasonAndSave(h.writes, 2); expect(h.writes[1].operation).toBe('revokeEnrollToken'); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); click('new-enroll-token'); await vi.waitFor(() => expect(document.querySelector('dialog h2')!.textContent).toBe('接入客户端'));
  });
  it('enrollment recognizes expiration, revocation, missing IDs and read failures while preserving once-only credentials', async () => {
    const h = harness(); h.data.nodes = []; await enroll(h.context); expect(document.querySelector('dialog')).toBeNull(); expect(document.body.textContent).toContain('没有可接入');
    const result = { id: 'tok', token: 'private once', expires_at: Date.now() + 60000, commands: { direct: 'test', unsupported: 42 } };
    const states: Json[] = [{ id: 'tok', used_at: null, revoked_at: 1, expires_at: 1 }, { id: 'tok', used_at: null, revoked_at: null, expires_at: 1 }, { id: 'tok', used_at: 1, client_id: '', expires_at: 1 }];
    for (const item of states) { h.data['enroll-tokens'] = [item]; const dialog = showEnrollment(h.context, { ...result }, 'node'); await vi.waitFor(() => expect(dialog.content.textContent).not.toContain('等待设备')); expect(dialog.content.textContent).toMatch(/已撤销|已过期|接入成功/); expect([...dialog.content.querySelectorAll('button')].find(control => control.textContent === '查看已接入资源')!.disabled).toBe(true); dialog.close(); }
    h.fetcher.mockRejectedValue(new Error('offline')); const dialog = showEnrollment(h.context, { ...result }, 'client'); await vi.waitFor(() => expect(dialog.content.textContent).toContain('暂时无法确认')); expect(document.querySelector<HTMLTextAreaElement>('dialog textarea')!.value).toBe('private once'); dialog.close();
  });
  it('paginates audit, stops at its bound and preserves older entries when the latest page changes', async () => {
    const h = harness(); const entries = (start: number) => Array.from({ length: 100 }, (_, index) => ({ id: start - index, ts: 1, actor: 'owner', action: 'future.action', target_kind: 'node', target_id: 'edge', reason: 'fixture', result: 'error', ip: '' }));
    h.data.audit = entries(10000); const page = auditPage(h.context); await mount(page, h.context); expect(document.body.textContent).toContain('100 条');
    for (let index = 1; index <= 9; index++) { h.data.audit = entries(10000 - index * 100); click('audit-older'); await vi.waitFor(() => expect(document.body.textContent).toContain(`本次加载 ${(index + 1) * 100} 条`)); }
    expect(document.querySelector<HTMLButtonElement>('[data-testid=audit-older]')!.disabled).toBe(true); expect(h.reads.some(path => path.includes('before=9901'))).toBe(true);
    h.data.audit = entries(11000); h.context.resources.invalidate(); await page.refresh(); expect(document.body.textContent).toContain('1000 条'); await page.refresh(true); expect(document.body.textContent).toContain('本次加载 100 条');
    h.fetcher.mockRejectedValue(new Error('offline')); click('audit-older'); await vi.waitFor(() => expect(h.context.showError).toHaveBeenCalled()); expect(document.body.textContent).toContain('本次加载 100 条');
  });
  it('honors settings permissions and protects the current and last owner', async () => {
    const viewer = harness('viewer'); expect(settingSections(viewer.context).map(([id]) => id)).toEqual(['account', 'appearance', 'about']);
    const h = harness(); await mount(settingsPage(h.context, 'admins'), h.context); click('edit-admin-a');
    expect(document.querySelector<HTMLSelectElement>('[name=role]')!.disabled).toBe(true); expect(document.querySelector<HTMLInputElement>('[name=disabled]')!.disabled).toBe(true);
    field('password', 'new test password'); await reasonAndSave(h.writes); expect(h.writes[0].args.body).toEqual({ password: 'new test password', reason: '维护验证' });
    await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); click('edit-admin-b'); field('role', 'operator'); await reasonAndSave(h.writes, 2); expect(h.writes[1].args.body?.role).toBe('operator');
  });
  it('creates and revokes API tokens with scopes and a one-time private dialog', async () => {
    const h = harness(); await mount(settingsPage(h.context, 'api'), h.context); click('new-api-token'); field('name', 'CI'); field('scope', 'tunnels'); await reasonAndSave(h.writes);
    await vi.waitFor(() => expect(document.querySelector('dialog h2')?.textContent).toBe('保存 API Token')); expect(document.querySelector<HTMLTextAreaElement>('dialog textarea')!.value).toBe('test-api-once');
    document.querySelector<HTMLDialogElement>('dialog')!.close(); expect(document.body.textContent).not.toContain('test-api-once'); click('revoke-api-token-api'); await reasonAndSave(h.writes, 2); expect(h.writes[1].operation).toBe('revokeApiToken');
  });
  it('creates enrollment with selected node and detects the one-time token being used', async () => {
    const h = harness(); await enroll(h.context); field('name', '开发设备'); field('node_id', 'edge'); await reasonAndSave(h.writes);
    expect(h.writes[0]).toMatchObject({ operation: 'createEnrollToken', args: { body: { node_id: 'edge', kind: 'client', ttl_minutes: 60 } } });
    await vi.waitFor(() => expect(document.querySelector('dialog h2')?.textContent).toBe('保存接入凭据')); document.querySelector<HTMLDialogElement>('dialog')!.close();
    h.data['enroll-tokens'] = [{ id: 'tok', name: '开发设备', kind: 'client', used_at: 1, client_id: 'c', expires_at: Date.now() + 60000, revoked_at: null }];
    const dialog = showEnrollment(h.context, { id: 'tok', token: 'test', expires_at: Date.now() + 60000, commands: { direct: 'test' } }, 'client'); await vi.waitFor(() => expect(dialog.content.textContent).toContain('接入成功'));
    [...dialog.content.querySelectorAll('button')].find(button => button.textContent === '查看已接入资源')!.click(); expect(h.context.navigate).toHaveBeenCalledWith('clients', 'c');
  });
  it('labels audit results and falls back to unknown action IDs without inventing meanings', async () => {
    const h = harness(); const page = auditPage(h.context); await mount(page, h.context); expect(page.element.textContent).toContain('修改客户端'); expect(page.element.textContent).toContain('client.update'); expect(page.element.textContent).toContain('本次加载 1 条');
    expect(actionLabel('unknown.new')).toBe('unknown.new'); expect(page.element.querySelector<HTMLButtonElement>('[data-testid=audit-older]')!.disabled).toBe(true);
    h.data.audit = [{ id: 15, ts: 1700000000000, actor: 'a', action: 'permission.denied', target_kind: '', target_id: '', reason: '', result: 'denied', ip: '' }]; await page.refresh(true); expect(page.element.textContent).toContain('拒绝'); expect(page.element.textContent).not.toContain('维护');
  });
  it('bounds logs, advances cursors and shows overflow gaps explicitly', async () => {
    const logs = new LogView(3); document.body.append(logs.element);
    logs.append({ items: ['a', 'b'], cursor: 2, gap: false, capacity: 128 }); logs.append({ items: ['c', 'd'], cursor: 4, gap: true, capacity: 128 }); expect(logs.cursor).toBe(4); expect(logs.element.children).toHaveLength(3); expect(logs.element.textContent).toContain('环形缓冲覆盖');
    expect(() => logs.append({ items: [1], cursor: 1 })).toThrow(); logs.clear(); expect(logs.cursor).toBe(0); expect(logs.element.children).toHaveLength(0);
    const h = harness(); await mount(settingsPage(h.context, 'logs'), h.context); expect(document.body.textContent).toContain('controller ready'); expect(h.reads).toContain('/api/v1/logs?after=0');
  });
});

describe('overview filtering and resource detail boundaries', () => {
  it('client filtering, missing nodes, invalid tags and migration eligibility remain accurate', async () => {
    const h = harness(); h.data.clients = [client('c', { tags: null, node_id: 'missing', last_ip: '' }), client('other', { tags: [], disabled_at: 1, node_id: 'edge' })]; await mount(clientsPage(h.context), h.context); expect(document.body.textContent).toContain('节点不可见'); const search = document.querySelector<HTMLInputElement>('[aria-label=搜索客户端]')!; search.value = '设备 other'; search.dispatchEvent(new Event('input')); expect(document.body.textContent).toContain('当前筛选 1 台'); click('detail-other'); expect(h.context.navigate).toHaveBeenCalledWith('clients', 'other');
    await mount(clientsPage(h.context, 'missing'), h.context); expect(document.body.textContent).toContain('客户端已删除'); [...document.querySelectorAll('button')].find(control => control.textContent === '返回列表')!.click(); expect(h.context.navigate).toHaveBeenCalledWith('clients');
    h.data.nodes = []; h.context.resources.invalidate(); await mount(clientsPage(h.context, 'c'), h.context); click('migrate-client'); expect(document.querySelector('dialog')).toBeNull(); expect(document.body.textContent).toContain('没有可迁移'); click('edit-client'); field('tags', Array.from({ length: 9 }, (_, index) => String(index)).join('\n')); field('reason', '测试'); submit(); await vi.waitFor(() => expect(document.querySelector('.form-error')!.textContent).toContain('标签最多')); expect(h.writes).toHaveLength(0);
    field('tags', 'x'.repeat(33)); submit(); await vi.waitFor(() => expect(document.querySelector<HTMLButtonElement>('[data-testid=dialog-submit]')!.disabled).toBe(false)); expect(h.writes).toHaveLength(0); field('tags', 'a\na'); submit(); await vi.waitFor(() => expect(h.writes).toHaveLength(1)); expect(h.writes[0].args.body?.tags).toEqual(['a']); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull());
    h.context.resources.invalidate(); await mount(clientsPage(h.context, 'other'), h.context); click('toggle-client'); await reasonAndSave(h.writes, 2); expect(h.writes[1].args.body?.disabled).toBe(false);
  });
  it('configuration import validates text, reports compatibility warnings and ignores stale or cancelled previews', async () => {
    const h = harness(); let dialog = configImport(h.context, 'c'); click('config-preview'); await vi.waitFor(() => expect(dialog.error.textContent).toContain('请填写'));
    field('text', 'x'.repeat(1024 * 1024 + 1)); click('config-preview'); await vi.waitFor(() => expect(dialog.error.textContent).toContain('不超过 1 MiB'));
    field('text', 'serverPort=7000'); h.fetcher.mockResolvedValueOnce(new Response(JSON.stringify({ digest: 'fixture', changes: [{ name: '更新项', kind: 'udp', action: 'update', local_ip: '::1', local_port: 0, remote_port: 0, enabled: false }, { name: '未来项', kind: 'tcp', action: 'future', local_ip: '', local_port: 0, remote_port: 0, enabled: true }], warnings: ['不支持字段', 2] }))); click('config-preview'); await vi.waitFor(() => expect(dialog.element.textContent).toContain('不支持字段')); expect(dialog.element.textContent).toContain('修改'); expect(dialog.element.textContent).toContain('future'); expect(dialog.submit.disabled).toBe(true);
    field('accept_warnings', true); field('reason', '兼容性检查'); submit(); await vi.waitFor(() => expect(h.writes).toHaveLength(1)); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull());
    dialog = configImport(h.context, 'c'); field('text', 'serverPort=7000'); let resolve!: (value: Response) => void; h.fetcher.mockImplementationOnce(() => new Promise(done => { resolve = done; })); click('config-preview'); field('text', 'serverPort=7001'); resolve(new Response(JSON.stringify({ digest: 'old', warnings: [], changes: [] }))); await vi.waitFor(() => expect(document.querySelector<HTMLButtonElement>('[data-testid=config-preview]')!.disabled).toBe(false)); expect(dialog.submit.disabled).toBe(true); expect(dialog.element.textContent).not.toContain('预览 0');
    h.fetcher.mockRejectedValueOnce(new Error('offline')); click('config-preview'); await vi.waitFor(() => expect(dialog.error.textContent).toContain('无法连接'));
    h.fetcher.mockImplementationOnce(() => new Promise(done => { resolve = done; })); click('config-preview'); dialog.close(); resolve(new Response('{}')); await Promise.resolve(); expect(document.querySelector('dialog')).toBeNull();
  });
  it('ranges and device filters use real API parameters and clear inaccessible resources', async () => {
    const h = harness(); const page = overviewPage(h.context); h.context.refresh = vi.fn(() => page.refresh(true)); await mount(page, h.context);
    expect(document.querySelectorAll('.metric-card')).toHaveLength(6); expect(document.body.textContent).toContain('暂无流量采样');
    const change = (id: string, selected: string) => { const control = document.querySelector<HTMLSelectElement>(`[data-testid=${id}]`)!; control.value = selected; control.dispatchEvent(new Event('change')); };
    change('traffic-client', 'c'); await vi.waitFor(() => expect(h.reads.some(path => path.includes('client_id=c'))).toBe(true)); change('traffic-tunnel', 't'); await vi.waitFor(() => expect(h.reads.some(path => path.includes('tunnel_id=t'))).toBe(true));
    document.querySelector<HTMLButtonElement>('[data-range="168"]')!.click(); await vi.waitFor(() => expect(h.reads.some(path => path.includes('hours=168'))).toBe(true));
    [...document.querySelectorAll('button')].find(control => control.textContent === '查看详情')!.click(); expect(h.context.navigate).toHaveBeenCalledWith('clients', 'c');
    h.data.clients = []; h.data.nodes = [node('edge', { online: false, disabled_at: 1 })]; h.data.tunnels = []; h.data.overview = { counts: {}, traffic: {}, traffic_24h: {}, version: '', process: null }; await page.refresh(true);
    expect(document.body.textContent).toContain('还没有客户端'); expect(document.body.textContent).toContain('已停用'); expect(document.querySelector<HTMLSelectElement>('[data-testid=traffic-client]')!.value).toBe(''); expect(document.querySelector<HTMLSelectElement>('[data-testid=traffic-tunnel]')!.value).toBe('');
    h.data.traffic = { range_hours: 24, window_end: 3600000, bucket_ms: 3600000, items: [{ timestamp: 3600000, bytes_in: 3, bytes_out: 4, connections_total: 1, gap: false }] }; await page.refresh(true); expect(document.querySelector('svg[data-testid=traffic-chart]')).not.toBeNull();
  });
  it('node lists preserve the empty built-in ID, missing details and operator permissions', async () => {
    const h = harness(); await mount(nodesPage(h.context), h.context); click('node-builtin'); expect(h.context.navigate).toHaveBeenCalledWith('nodes', ''); click('new-node'); field('name', '公网节点'); await reasonAndSave(h.writes); expect(h.writes[0].args.body?.kind).toBe('node'); await vi.waitFor(() => expect(document.querySelector('dialog h2')!.textContent).toBe('保存接入凭据')); document.querySelector<HTMLDialogElement>('dialog')!.close();
    await mount(nodesPage(h.context, 'missing'), h.context); expect(document.body.textContent).toContain('节点已删除'); [...document.querySelectorAll('button')].find(control => control.textContent === '返回节点列表')!.click(); expect(h.context.navigate).toHaveBeenCalledWith('nodes');
    const viewer = harness('viewer'); await mount(nodesPage(viewer.context, 'edge'), viewer.context); expect(document.querySelector('[data-testid=rename-node]')).toBeNull();
  });
  it('tunnel details hide secret values, preserve edit contracts and handle unknown owners and deleted IDs', async () => {
    const h = harness(); h.data.tunnels = [tunnel('t', { enabled: false, use_encryption: true, use_compression: true, max_connections: 10, http_password_set: true, lb_group_key_set: true, http_password: 'never render password', lb_group_key: 'never render group' })]; await mount(tunnelsPage(h.context, 't'), h.context);
    expect(document.body.textContent).toContain('已停用'); expect(document.body.textContent).not.toContain('never render'); click('edit-t'); await vi.waitFor(() => expect(document.querySelector('dialog h2')!.textContent).toBe('编辑隧道')); field('name', '新的名称'); await reasonAndSave(h.writes); expect(h.writes[0].operation).toBe('patchTunnel'); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull());
    [...document.querySelectorAll('button')].find(control => control.textContent === '返回隧道列表')!.click(); expect(h.context.navigate).toHaveBeenCalledWith('tunnels');
    h.data.clients = []; h.data.nodes = []; h.context.resources.invalidate(); await mount(tunnelsPage(h.context, 't'), h.context); expect(document.body.textContent).toContain('未知客户端'); await mount(tunnelsPage(h.context, 'missing'), h.context); expect(document.body.textContent).toContain('隧道已删除');
    const viewer = harness('viewer'); await mount(tunnelsPage(viewer.context, 't'), viewer.context); expect(document.querySelector('[data-testid=edit-t]')).toBeNull();
  });
  it('tunnel filters, checkbox changes and empty batches retain a bounded accurate selection', async () => {
    const h = harness(); h.data.tunnels = [tunnel('t', { state: 'error', status_error: 'port already used' }), tunnel('off', { enabled: false, state: 'stopped', client_id: 'other' })]; const page = tunnelsPage(h.context); await mount(page, h.context);
    const choose = (id: string, selected: string) => { const control = document.querySelector<HTMLSelectElement>(`[data-testid=${id}]`)!; control.value = selected; control.dispatchEvent(new Event('change')); };
    choose('tunnel-state-filter', 'disabled'); expect(document.body.textContent).toContain('已停用'); expect(document.body.textContent).not.toContain('port already used'); choose('tunnel-state-filter', 'error'); expect(document.body.textContent).toContain('port already used');
    const check = document.querySelector<HTMLInputElement>('[data-testid=select-t]')!; check.checked = true; check.dispatchEvent(new Event('change')); expect(document.body.textContent).toContain('已选择 1 条'); document.querySelector<HTMLInputElement>('[data-testid=select-t]')!.click(); expect(document.body.textContent).toContain('已选择 0 条');
    choose('tunnel-state-filter', 'all'); choose('tunnel-client-filter', 'other'); expect(document.body.textContent).toContain('当前筛选 1 条'); [...document.querySelectorAll('button')].find(control => control.textContent === '选择当前筛选')!.click(); expect(document.body.textContent).toContain('已选择 1 条'); [...document.querySelectorAll('button')].find(control => control.textContent === '清空选择')!.click(); expect(document.body.textContent).toContain('已选择 0 条');
    click('toggle-off'); await reasonAndSave(h.writes); expect(h.writes[0].args.body?.enabled).toBe(true); await vi.waitFor(() => expect(document.querySelector('dialog')).toBeNull()); click('delete-off'); await reasonAndSave(h.writes, 2); expect(h.writes[1].operation).toBe('deleteTunnel');
  });
});
