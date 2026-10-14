import type { Context, Page } from '../app/context';
import { address, allowed, date, disabled, records, status, type Client, type Tunnel, type Visitor, type NodeInfo } from '../api/models';
import { badge, button, details, el, intro, panel, toast, value } from '../components/dom';
import { confirmation } from '../components/forms';
import { edit, guarded, View } from '../components/operations';
import { ResourceTable } from '../components/table';
import { enroll } from './enrollment';
import { configImport, configExport } from './config-transfer';
import { tunnelEditor, tunnelRows } from './tunnels';
import { visitorEditor, visitorRows } from './visitors';

export function clientsPage(context: Context, id?: string): Page {
  const element = el('div', 'page-content'); const summary = el('div'); const view = new View(summary); const manage = allowed(context.admin?.role, 'admin'); const operator = allowed(context.admin?.role, 'operator');
  const table = new ResourceTable(['名称 / 标签', '连接状态', '系统 / Agent', '服务端节点', '最近连接 / IP', '操作'], 1000); table.element.classList.add('mobile-cards');
  const tunnelTable = new ResourceTable(['名称 / 类型', '状态', '客户端 / 节点', '公网入口', '本地目标', '操作'], 1120); tunnelTable.element.classList.add('mobile-cards');
  const visitorTable = new ResourceTable(['访问者 / 本机端口', '目标隧道 / 客户端', '状态 / 传输', '操作'], 820);
  const tunnelPanel = panel('客户端隧道', 'tunnels', tunnelTable.element); const visitorPanel = panel('访问者', 'transfer', visitorTable.element);
  const actions = manage && id === undefined ? [button('接入客户端', guarded(context, () => enroll(context)), { primary: true, icon: 'plus', id: 'new-client' })] : [];
  const search = el('input'); search.type = 'search'; search.placeholder = '搜索名称、标签或 IP'; search.setAttribute('aria-label', '搜索客户端');
  const count = el('span', 'muted-text'); let clients: Client[] = []; let nodes: NodeInfo[] = []; let disposed = false;
  element.append(intro('客户端设备', '管理已接入设备、隧道与访问者；在线状态来自服务端实时连接。', 'clients'), summary);
  if (id === undefined) element.append(el('div', 'filters', search, count), table.element); else element.append(tunnelPanel, visitorPanel);
  const render = () => {
    const query = search.value.toLowerCase(); const items = clients.filter(client => [client.name, ...(client.tags ?? []), client.last_ip].join(' ').toLowerCase().includes(query));
    count.textContent = `当前筛选 ${items.length} 台 / 已加载 ${clients.length} 台`;
    table.update(items.map(client => ({ id: client.id, signature: JSON.stringify([client, nodes]), cells: () => {
      const [state, tone] = status(client); const node = nodes.find(node => node.id === client.node_id);
      return [value(client.name, client.tags?.join(' · ') || client.id), badge(state, tone), value(`${client.os} ${client.arch}`, `Agent ${client.version}`), value(node?.builtin ? '内置节点' : node?.name, node ? address(node.public_host, node.edge_port) : '节点不可见', 'blue'), value(date(client.last_seen_at), client.last_ip), button('查看详情', () => context.navigate('clients', client.id), { id: `detail-${client.id}` })];
    }})));
  };
  search.addEventListener('input', render);
  const controls = (client: Client) => {
    const container = el('div', 'actions');
    if (operator) container.append(button('导入配置', () => { configImport(context, client.id); }, { id: 'import-config' }));
    if (manage) container.append(button('修改设备', () => { edit('修改客户端', [
      { key: 'name', label: '名称', value: client.name, required: true, maxLength: 64 },
      { key: 'tags', label: '标签', kind: 'list', value: client.tags ?? [], description: '最多 8 个，每个 1–32 个字符，每行一个；重复标签自动合并。' },
      { key: 'max_connections', label: '全部隧道共享并发上限', kind: 'number', value: client.max_connections ?? 0, min: 0, max: 1000000, description: '流 / 活跃 UDP 源共享限制；0 不限。' },
    ], async body => { const tags = Array.from(new Set(Array.isArray(body.tags) ? body.tags : [])); if (tags.length > 8 || tags.some(tag => typeof tag !== 'string' || !tag || Array.from(tag).length > 32 || /[\u0000-\u001f\u007f-\u009f]/u.test(tag))) throw new Error('标签最多 8 个，每个 1–32 个可打印字符。'); body.tags = tags; await context.write('patchClient', { id: client.id, body }); }); }, { id: 'edit-client' }),
      button(disabled(client) ? '启用客户端' : '停用客户端', () => { confirmation(disabled(client) ? '启用客户端' : '停用客户端', disabled(client) ? '启用后允许该设备重新连接。' : '停用后立即断开该设备及其隧道，并拒绝重新连接。', reason => context.write('patchClient', { id: client.id, body: { disabled: !disabled(client), reason } }).then(() => {})); }, { id: 'toggle-client' }),
      button('强制下线', () => { confirmation('强制下线', '当前连接会断开，Agent 随后可自动重连。', reason => context.write('kickClient', { id: client.id, body: { reason } }).then(() => {})); }, { disabled: !client.online, id: 'kick-client' }),
      button('重启 Agent', () => { confirmation('重启 Agent', 'Agent 以退出码 75 退出，需由服务管理器重新拉起。隧道将短暂中断。', async reason => { await context.write('restartClient', { id: client.id, body: { reason } }); toast('已发送重启指令'); }); }, { disabled: !client.online, id: 'restart-client' }),
      button('更新 Agent', () => { edit('下发 Agent 更新', [{ key: 'version', label: 'Release tag', required: true, maxLength: 64, description: '例如 v1.2.3。Agent 校验签名和摘要后替换，由后台服务重新启动。' }], body => context.write('updateClient', { id: client.id, body }), '下发更新'); }, { disabled: !client.online, id: 'update-client' }),
      button('迁移服务端节点', () => {
        const choices = nodes.filter(node => node.id !== client.node_id && !disabled(node) && node.public_host && node.edge_port);
        if (!choices.length) { toast('没有可迁移的目标节点。', 'warning'); return; }
        edit('迁移客户端', [{ key: 'node_id', label: '目标节点', kind: 'select', value: choices[0]?.id ?? '', options: choices.map(node => [node.id, `${node.builtin ? '内置节点' : node.name} · ${node.online ? '在线' : '离线'}`]), description: '设备会切换到目标 Edge；隧道短暂中断。关联私密隧道与访问者须满足同节点约束。' }], body => context.write('migrateClient', { id: client.id, body }), '迁移');
      }, { disabled: !client.online, id: 'migrate-client' }),
      button('导出配置', guarded(context, () => configExport(context, client.id)), { id: 'export-config' }),
      button('删除客户端', () => { confirmation('删除客户端', `删除「${client.name}」及其全部隧道与访问者。设备需要重新接入。`, async reason => { await context.write('deleteClient', { id: client.id, query: { reason } }); context.navigate('clients'); }); }, { danger: true, id: 'delete-client' }));
    return container;
  };
  async function refresh(force = false): Promise<void> {
    const age = force ? 0 : 4000;
    const promises = [context.resources.read('clients', 'clients', {}, age), context.resources.read('nodes', 'nodes', {}, age)];
    if (id !== undefined) promises.push(context.resources.read('tunnels', 'tunnels', {}, age), context.resources.read('visitors', 'visitors', {}, age));
    const values = await Promise.all(promises); if (disposed) return;
    clients = records<Client>(values[0]); nodes = records<NodeInfo>(values[1]);
    if (id === undefined) { render(); return; }
    const client = clients.find(client => client.id === id); const node = nodes.find(node => node.id === client?.node_id);
    view.update([client ?? null, node ?? null], () => {
      const back = button('返回列表', () => context.navigate('clients'), { icon: 'back' });
      if (!client) return [back, panel('客户端已删除或不可见', 'clients')];
      const [state, tone] = status(client);
      return [back, intro(client.name, client.tags?.join(' · ') || '未设置标签', 'clients', badge(state, tone)), el('div', 'detail-grid',
        panel('设备信息', 'clients', details([['设备 ID', client.id], ['操作系统', client.os], ['架构', client.arch], ['Agent 版本', client.version], ['配置版本', String(client.revision)], ['创建时间', date(client.created_at)]])),
        panel('连接与接入', 'transfer', details([['连接状态', state], ['最近连接', date(client.last_seen_at)], ['登录 IP', client.last_ip], ['服务端节点', node?.builtin ? '内置节点' : node?.name], ['接入地址', node ? address(node.public_host, node.edge_port) : '节点不可见'], ['共享并发上限', client.max_connections ? String(client.max_connections) : '不限']]))),
        panel('设备管理', 'settings', controls(client), !manage ? el('p', 'muted-text', operator ? '运维账号可管理隧道、访问者及配置导入。设备变更需管理员。' : '当前账号为只读权限。') : null)];
    });
    tunnelPanel.hidden = !client; visitorPanel.hidden = !client;
    if (!client) return;
    const tunnels = records<Tunnel>(values[2]); const visitors = records<Visitor>(values[3]);
    const heading = tunnelPanel.querySelector('h3');
    if (heading && !heading.querySelector('button') && operator) heading.append(button('创建隧道', guarded(context, () => tunnelEditor(context, undefined, id)), { compact: true, id: 'client-new-tunnel' }));
    const visitorHeading = visitorPanel.querySelector('h3');
    if (visitorHeading && !visitorHeading.querySelector('button') && operator) visitorHeading.append(button('创建访问者', guarded(context, () => visitorEditor(context, id)), { compact: true, id: 'client-new-visitor' }));
    tunnelTable.update(tunnelRows(context, tunnels.filter(tunnel => tunnel.client_id === id), clients, nodes));
    visitorTable.update(visitorRows(context, visitors.filter(visitor => visitor.client_id === id), clients, tunnels));
  }
  return { element, actions, refresh, dispose() { disposed = true; table.dispose(); tunnelTable.dispose(); visitorTable.dispose(); } };
}
