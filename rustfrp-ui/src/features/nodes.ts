import type { Context, Page } from '../app/context';
import { address, allowed, date, disabled, records, status, text, type NodeInfo } from '../api/models';
import { badge, button, details, el, intro, panel, value } from '../components/dom';
import { confirmation } from '../components/forms';
import { edit, guarded, View } from '../components/operations';
import { ResourceTable } from '../components/table';
import { enroll } from './enrollment';

export function nodeCapabilities(node: NodeInfo): string {
  return ['TCP / UDP', node.vhost_http ? `HTTP ${node.vhost_http}` : '', node.vhost_https ? `HTTPS ${node.vhost_https}` : '', node.tcpmux_http_connect_port ? `TCPMUX ${node.tcpmux_http_connect_port}` : '', node.subdomain_host ? `子域名 ${node.subdomain_host}` : ''].filter(Boolean).join(' · ');
}
export function nodePorts(node: NodeInfo): string {
  return node.allow_ports?.length ? node.allow_ports.map(([from, to]) => from === to ? String(from) : `${from}–${to}`).join('、') : '不限';
}
export function nodesPage(context: Context, id?: string): Page {
  const element = el('div', 'page-content'); const summary = el('div'); const view = new View(summary);
  const canManage = allowed(context.admin?.role, 'admin');
  const table = new ResourceTable(['名称 / 类型', '状态', '客户端', '公网接入地址', '版本 / 能力', '操作'], 980);
  element.append(intro('服务端节点', '服务端 Edge 提供客户端接入与隧道转发。内置节点与 Controller 同进程。', 'nodes'), summary);
  if (id === undefined) element.append(table.element);
  const actions = id === undefined && canManage ? [button('接入服务端节点', guarded(context, () => enroll(context, 'node')), { primary: true, icon: 'plus', id: 'new-node' })] : [];
  let disposed = false;
  async function refresh(force = false): Promise<void> {
    const nodes = records<NodeInfo>(await context.resources.read('nodes', 'nodes', {}, force ? 0 : 4000));
    if (disposed) return;
    if (id === undefined) {
      view.update(nodes.length, () => [el('p', 'muted-text', `已加载 ${nodes.length} 个服务端节点。客户端列标明各节点返回的统计口径。`)]);
      table.update(nodes.map(node => ({ id: node.id || 'builtin', signature: JSON.stringify(node), cells: () => {
        const [state, tone] = status(node);
        return [value(node.builtin ? '内置节点' : node.name, node.builtin ? '内置服务端' : '独立服务端', node.builtin ? 'success' : 'blue'), badge(node.builtin && node.online ? '运行中' : state, tone), value(String(node.clients), '已接入客户端'), value(address(node.public_host, node.edge_port), undefined, 'blue'), value(node.version, nodeCapabilities(node)), button('查看详情', () => context.navigate('nodes', node.id), { id: `node-${node.id || 'builtin'}` })];
      }})));
      return;
    }
    const node = nodes.find(node => node.id === id);
    view.update(node ?? null, () => {
      const back = button('返回节点列表', () => context.navigate('nodes'), { icon: 'back' });
      if (!node) return [back, panel('节点已删除或不可见', 'nodes', el('p', 'muted-text', '刷新列表查看可访问的节点。'))];
      const [state, tone] = status(node);
      const controls = el('div', 'actions');
      if (canManage && !node.builtin) {
        controls.append(button('修改名称', () => { edit('修改节点名称', [{ key: 'name', label: '名称', value: node.name, required: true, maxLength: 64 }], body => context.write('patchNode', { id: node.id, body })); }, { id: 'rename-node' }),
          button(disabled(node) ? '启用节点' : '停用节点', () => { confirmation(disabled(node) ? '启用节点' : '停用节点', '停用后拒绝新的客户端接入；控制连接断开不代表所有已有隧道立即停止。', reason => context.write('patchNode', { id: node.id, body: { disabled: !disabled(node), reason } }).then(() => {})); }, { id: 'toggle-node' }),
          button('删除节点', () => { confirmation('删除节点', '节点必须已停用且没有关联客户端。删除后需重新接入才能继续管理。', async reason => { await context.write('deleteNode', { id: node.id, query: { reason } }); context.navigate('nodes'); }); }, { danger: true, disabled: !disabled(node) || node.clients > 0, id: 'delete-node' }));
      }
      return [back, intro(node.name, node.builtin ? '内置服务端由 Controller 管理。' : '独立服务端可在控制面断开时继续转发已建立的连接。', 'nodes', badge(state, tone)), el('div', 'detail-grid',
        panel('运行状态', 'activity', details([['类型', node.builtin ? '内置服务端' : '独立服务端'], ['状态', state], ['已接入客户端', String(node.clients)], ['Agent 版本', node.version], ['最近在线', date(node.last_seen_at)]])),
        panel('网络与接入', 'transfer', details([['公网接入地址', address(node.public_host, node.edge_port)], ['远程端口范围', nodePorts(node)]])),
        panel('服务能力', 'tunnels', details([['HTTP 端口', node.vhost_http ? String(node.vhost_http) : '未配置'], ['HTTPS 端口', node.vhost_https ? String(node.vhost_https) : '未配置'], ['TCPMUX 端口', node.tcpmux_http_connect_port ? String(node.tcpmux_http_connect_port) : '未配置'], ['子域名后缀', text(node.subdomain_host, '未配置')]])),
        panel('身份与安全', 'key', details([['节点 ID', node.id || '内置节点'], ['证书 SHA-256', text(node.tls_fingerprint, '未上报')], ['创建时间', date(node.created_at)]]))),
        panel('节点管理', 'settings', controls, el('p', 'muted-text', node.builtin ? '内置服务端的启动配置通过 Controller 环境配置调整。' : canManage ? '所有修改需要操作理由并记录审计。' : '当前账号拥有只读权限。'))];
    });
  }
  return { element, actions, refresh, dispose() { disposed = true; table.dispose(); } };
}
