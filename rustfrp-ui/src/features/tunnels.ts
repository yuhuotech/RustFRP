import type { Context, Page } from '../app/context';
import { allowed, date, records, publicAddress, status, text, type Client, type Tunnel, type NodeInfo } from '../api/models';
import { badge, button, details, el, intro, panel, select, toast, value } from '../components/dom';
import { confirmation, FormDialog, reason } from '../components/forms';
import { guarded, View } from '../components/operations';
import { ResourceTable, type Row } from '../components/table';
import { templates, tunnelBody, tunnelFields } from './tunnel-spec';

export async function tunnelEditor(context: Context, existing?: Tunnel, clientId?: string): Promise<void> {
  const focus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const clients = records<Client>(await context.resources.read('clients', 'clients', {}, 4000));
  if (!clients.length) { toast('请先接入客户端。', 'warning'); return; }
  const fields = tunnelFields(clients.map(client => [client.id, client.name]), existing, clientId);
  const dialog = new FormDialog(existing ? '编辑隧道' : '创建隧道', [...fields, { ...reason, group: '操作确认' }], async (values, current) => {
    const body = tunnelBody(values, existing, clientId);
    await context.write(existing ? 'patchTunnel' : 'createTunnel', { id: existing?.id, body }); current.finish(); toast(existing ? '隧道已保存' : '隧道已创建');
  }, '保存', focus);
  if (!existing) {
    const template = select([['custom', '自定义'], ...Object.entries(templates).map(([key, [name]]) => [key, name] as [string, string])], 'custom', choice => {
      const spec = templates[choice]; if (!spec) return;
      dialog.set('name', spec[0]); dialog.set('local_port', spec[1]); dialog.set('kind', spec[2]); dialog.set('local_ip', '127.0.0.1'); dialog.set('remote_port', 0); dialog.dirty = true; update();
    }, 'tunnel-template');
    dialog.content.prepend(el('div', 'form-field', el('label', '', '常用模板'), template));
  }
  const update = () => {
    const kind = existing?.kind ?? dialog.control('kind')?.value ?? 'tcp';
    const balanced = ['tcp', 'http', 'https'].includes(kind); const datagram = ['udp', 'sudp'].includes(kind);
    dialog.showGroup('域名', ['http', 'https', 'tcpmux'].includes(kind)); dialog.showGroup('HTTP 路由与认证', kind === 'http'); dialog.showGroup('负载均衡与健康检查', balanced);
    dialog.showField('remote_port', ['tcp', 'udp'].includes(kind)); dialog.showField('multiplexer', kind === 'tcpmux'); dialog.showField('proxy_protocol', balanced);
    dialog.showField('bandwidth_bps', !datagram); dialog.showField('bandwidth_server', !datagram); dialog.showField('clear_http_password', !!existing);
    dialog.showField('plugin_json', dialog.control('plugin_action')?.value === 'replace');
    const hc = dialog.control('hc_kind')?.value ?? '';
    for (const key of ['hc_timeout', 'hc_failed', 'hc_interval']) dialog.showField(key, !!hc);
    for (const key of ['hc_path', 'hc_headers']) dialog.showField(key, hc === 'http');
  };
  dialog.form.addEventListener('change', update); update();
}
export function tunnelRows(context: Context, tunnels: Tunnel[], clients: Client[], nodes: NodeInfo[], selected?: Set<string>, change?: (id: string, checked: boolean) => void): Row[] {
  const manage = allowed(context.admin?.role, 'operator');
  return tunnels.map(tunnel => ({ id: tunnel.id, signature: JSON.stringify([tunnel, clients.find(client => client.id === tunnel.client_id), nodes, selected?.has(tunnel.id)]), cells: () => {
    const owner = clients.find(client => client.id === tunnel.client_id); const node = nodes.find(node => node.id === (owner?.node_id ?? ''));
    const [state, tone] = status(tunnel); const stateCell = el('div', '', badge(tunnel.enabled ? state : '已停用', tunnel.enabled ? tone : 'muted'));
    if (tunnel.status_error) stateCell.append(value(tunnel.status_error, undefined, 'danger'));
    const ops = el('div', 'actions', button('详情', () => context.navigate('tunnels', tunnel.id), { id: `detail-${tunnel.id}` }));
    if (manage) ops.append(button(tunnel.enabled ? '停用' : '启用', () => { confirmation(tunnel.enabled ? '停用隧道' : '启用隧道', `操作隧道「${tunnel.name}」，配置将下发到客户端。`, reason => context.write('patchTunnel', { id: tunnel.id, body: { enabled: !tunnel.enabled, reason } }).then(() => {})); }, { id: `toggle-${tunnel.id}`, compact: true }),
      button('编辑', guarded(context, () => tunnelEditor(context, tunnel)), { id: `edit-${tunnel.id}`, compact: true }),
      button('删除', () => { confirmation('删除隧道', '删除后隧道停止，关联访问者会同时移除。', async reason => { await context.write('deleteTunnel', { id: tunnel.id, query: { reason } }); }); }, { danger: true, id: `delete-${tunnel.id}`, compact: true }));
    const name = value(tunnel.name, tunnel.kind.toUpperCase());
    if (selected && change && manage) {
      const checkbox = el('input'); checkbox.type = 'checkbox'; checkbox.checked = selected.has(tunnel.id); checkbox.setAttribute('aria-label', `选择 ${tunnel.name}`);
      checkbox.dataset.testid = `select-${tunnel.id}`; checkbox.addEventListener('change', () => change(tunnel.id, checkbox.checked)); name.prepend(checkbox);
    }
    return [name, stateCell, value(owner?.name, node?.builtin ? '内置节点' : node?.name), value(publicAddress(tunnel, node), undefined, 'blue'), value(`${tunnel.local_ip}:${tunnel.local_port}`), ops];
  }}));
}
export function tunnelsPage(context: Context, id?: string): Page {
  const element = el('div', 'page-content'); const table = new ResourceTable(['名称 / 类型', '实际状态', '客户端 / 节点', '公网入口', '本地目标', '操作'], 1120);
  table.element.classList.add('mobile-cards');
  const detail = el('div'); const view = new View(detail); const summary = el('p', 'muted-text'); const selection = el('div', 'filters');
  const selected = new Set<string>(); const manage = allowed(context.admin?.role, 'operator'); let filter = 'all'; let owner = ''; let all: Tunnel[] = []; let clients: Client[] = []; let nodes: NodeInfo[] = []; let disposed = false;
  const ownerFilter = select([['', '全部客户端']], '', val => { owner = val; render(); }, 'tunnel-client-filter');
  const filters = el('div', 'filters', select([['all', '全部状态'], ['running', '运行中'], ['error', '异常'], ['pending', '待启动'], ['stopped', '已停止'], ['disabled', '已停用']], 'all', val => { filter = val; render(); }, 'tunnel-state-filter'), ownerFilter, summary);
  element.append(intro('隧道与公网服务', '查看期望配置与实际运行状态；批量操作每次最多 100 条。', 'tunnels'), detail);
  if (id === undefined) element.append(filters, selection, table.element);
  const change = (id: string, checked: boolean) => {
    if (checked && selected.size >= 100) toast('一次最多选择 100 条隧道。', 'warning'); else if (checked) selected.add(id); else selected.delete(id); render();
  };
  const batch = (enabled: boolean) => {
    const ids = [...selected]; if (!ids.length) return;
    confirmation(enabled ? '批量启用隧道' : '批量停用隧道', `将操作已选择的 ${ids.length} 条隧道（包含当前筛选外的选择）。`, async reason => {
      await context.write('batchTunnels', { body: { ids, enabled, reason } }); selected.clear(); render(); toast(`已更新 ${ids.length} 条隧道`);
    });
  };
  function render(): void {
    if (id !== undefined) return;
    const items = all.filter(tunnel => (!owner || tunnel.client_id === owner) && (filter === 'all' || (filter === 'disabled' ? !tunnel.enabled : tunnel.enabled && tunnel.state === filter)));
    for (const item of selected) if (!all.some(tunnel => tunnel.id === item)) selected.delete(item);
    summary.textContent = `当前筛选 ${items.length} 条 / 已加载 ${all.length} 条`;
    selection.replaceChildren();
    if (manage) selection.append(button('选择当前筛选', () => { for (const tunnel of items) { if (selected.size >= 100) break; selected.add(tunnel.id); } render(); }), el('span', '', `已选择 ${selected.size} 条`), button('批量启用', () => batch(true), { disabled: selected.size === 0 }), button('批量停用', () => batch(false), { disabled: selected.size === 0 }), button('清空选择', () => { selected.clear(); render(); }, { disabled: selected.size === 0 }));
    table.update(tunnelRows(context, items, clients, nodes, selected, change));
  }
  async function refresh(force = false): Promise<void> {
    const age = force ? 0 : 4000; const values = await Promise.all([context.resources.read('tunnels', 'tunnels', {}, age), context.resources.read('clients', 'clients', {}, age), context.resources.read('nodes', 'nodes', {}, age)]);
    if (disposed) return;
    all = records<Tunnel>(values[0]); clients = records<Client>(values[1]); nodes = records<NodeInfo>(values[2]);
    const choices = [['', '全部客户端'], ...clients.map(client => [client.id, client.name])];
    const signature = JSON.stringify(choices);
    if (ownerFilter.dataset.options !== signature) { ownerFilter.replaceChildren(...choices.map(([val, label]) => { const option = el('option', '', label); option.value = val; return option; })); ownerFilter.value = owner; ownerFilter.dataset.options = signature; }
    if (id === undefined) { render(); return; }
    const tunnel = all.find(tunnel => tunnel.id === id); const client = clients.find(client => client.id === tunnel?.client_id); const node = nodes.find(node => node.id === (client?.node_id ?? ''));
    view.update([tunnel ?? null, client ?? null, node ?? null], () => {
      const back = button('返回隧道列表', () => context.navigate('tunnels'), { icon: 'back' });
      if (!tunnel) return [back, panel('隧道已删除或不可见', 'tunnels')];
      const [state, tone] = status(tunnel);
      const controls = el('div', 'actions');
      if (manage) controls.append(button('编辑隧道', guarded(context, () => tunnelEditor(context, tunnel)), { primary: true, id: `edit-${tunnel.id}` }));
      const config = { ...tunnel }; delete config.http_password; delete config.lb_group_key;
      const json = el('pre', 'readonly-text', JSON.stringify(config, null, 2));
      return [back, intro(tunnel.name, `${tunnel.kind.toUpperCase()} · ${client?.name ?? '未知客户端'}`, 'tunnels', badge(tunnel.enabled ? state : '已停用', tone)), controls, el('div', 'detail-grid',
        panel('运行与连接', 'activity', details([['实际状态', state], ['错误', text(tunnel.status_error, '无')], ['公网入口', publicAddress(tunnel, node)], ['本地目标', `${tunnel.local_ip}:${tunnel.local_port}`], ['创建时间', date(tunnel.created_at)], ['更新时间', date(tunnel.updated_at)]])),
        panel('传输与安全', 'key', details([['附加加密', tunnel.use_encryption ? '开启' : '关闭'], ['压缩', tunnel.use_compression ? '开启' : '关闭'], ['连接上限', tunnel.max_connections ? String(tunnel.max_connections) : '不限'], ['HTTP 密码', tunnel.http_password_set ? '已设置' : '未设置'], ['组密钥', tunnel.lb_group_key_set ? '已设置' : '未设置']]))), panel('完整配置摘要', 'tunnels', json)];
    });
  }
  return { element, actions: manage && id === undefined ? [button('创建隧道', guarded(context, () => tunnelEditor(context)), { primary: true, icon: 'plus', id: 'new-tunnel' })] : [], refresh, dispose() { disposed = true; selected.clear(); table.dispose(); } };
}
