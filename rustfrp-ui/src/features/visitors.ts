import type { Context } from '../app/context';
import { allowed, records, text, type Client, type Tunnel, type Visitor } from '../api/models';
import { badge, button, el, toast, value } from '../components/dom';
import { confirmation } from '../components/forms';
import { edit, guarded } from '../components/operations';
import type { Row } from '../components/table';

export function visitorTargets(clientId: string, clients: Client[], tunnels: Tunnel[]): Tunnel[] {
  const client = clients.find(item => item.id === clientId);
  if (!client) return [];
  return tunnels.filter(tunnel => ['stcp', 'sudp'].includes(tunnel.kind) && tunnel.client_id !== clientId && clients.some(owner => owner.id === tunnel.client_id && owner.node_id === client.node_id));
}
export async function visitorEditor(context: Context, clientId: string, existing?: Visitor): Promise<void> {
  const focus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const [clientValues, tunnelValues] = await Promise.all([context.resources.read('clients', 'clients', {}, 4000), context.resources.read('tunnels', 'tunnels', {}, 4000)]);
  const clients = records<Client>(clientValues); const tunnels = records<Tunnel>(tunnelValues); const targets = visitorTargets(clientId, clients, tunnels);
  if (!targets.length) { toast('同一节点上没有其他客户端提供的 STCP / SUDP 隧道，请先创建目标隧道。', 'warning'); return; }
  const options: [string, string][] = targets.map(tunnel => [tunnel.id, `${clients.find(client => client.id === tunnel.client_id)?.name} / ${tunnel.name} · ${tunnel.kind.toUpperCase()}`]);
  edit(existing ? '编辑访问者' : '创建访问者', [
    { key: 'name', label: '名称', value: existing?.name ?? '', required: true, maxLength: 64 },
    { key: 'tunnel_id', label: '目标私密隧道', kind: 'select', options, value: existing?.tunnel_id ?? targets[0]?.id ?? '', required: true, description: '只能访问同一节点上其他设备的 STCP / SUDP 隧道，密钥由 Controller 下发。' },
    { key: 'bind_addr', label: '本机监听地址', value: existing?.bind_addr ?? '127.0.0.1', required: true },
    { key: 'bind_port', label: '本机端口', kind: 'number', value: existing?.bind_port ?? 0, min: 0, max: 65535, description: '0 表示自动分配。' },
    { key: 'use_encryption', label: '附加加密', kind: 'checkbox', value: existing?.use_encryption ?? false },
    { key: 'use_compression', label: '压缩', kind: 'checkbox', value: existing?.use_compression ?? false },
    { key: 'enabled', label: '启用访问者', kind: 'checkbox', value: existing?.enabled ?? true },
  ], body => context.write(existing ? 'patchVisitor' : 'createVisitor', { id: existing?.id, body: existing ? body : { ...body, client_id: clientId } }), '保存', focus);
}
export function visitorRows(context: Context, visitors: Visitor[], clients: Client[], tunnels: Tunnel[]): Row[] {
  const manage = allowed(context.admin?.role, 'operator');
  return visitors.map(visitor => ({ id: visitor.id, signature: JSON.stringify([visitor, tunnels.find(tunnel => tunnel.id === visitor.tunnel_id), clients]), cells: () => {
    const target = tunnels.find(tunnel => tunnel.id === visitor.tunnel_id); const owner = clients.find(client => client.id === target?.client_id);
    const actions = el('div', 'actions');
    if (manage) actions.append(button(visitor.enabled ? '停用' : '启用', () => { confirmation(visitor.enabled ? '停用访问者' : '启用访问者', `更新「${visitor.name}」的本机监听与转发状态。`, reason => context.write('patchVisitor', { id: visitor.id, body: { enabled: !visitor.enabled, reason } }).then(() => {})); }, { id: `toggle-visitor-${visitor.id}` }),
      button('编辑', guarded(context, () => visitorEditor(context, visitor.client_id, visitor)), { id: `edit-visitor-${visitor.id}` }),
      button('删除', () => { confirmation('删除访问者', '删除后不再监听该访问者的本机端口。', reason => context.write('deleteVisitor', { id: visitor.id, query: { reason } }).then(() => {})); }, { danger: true, id: `delete-visitor-${visitor.id}` }));
    return [value(visitor.name, `${visitor.bind_addr}:${visitor.bind_port || '自动'}`), value(target?.name ?? '目标已删除', text(owner?.name)), el('div', 'actions', badge(visitor.enabled ? '已启用' : '已停用', visitor.enabled ? 'success' : 'muted'), visitor.use_encryption ? badge('加密', 'blue') : null, visitor.use_compression ? badge('压缩', 'success') : null), actions];
  }}));
}
