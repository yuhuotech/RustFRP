import { ApiError } from '../api/client';
import { allowed, disabled, record, records, text, type NodeInfo } from '../api/models';
import type { Context } from '../app/context';
import { el, toast } from '../components/dom';
import { FormDialog, reason } from '../components/forms';

// The authenticated Controller supplies the address and certificate; tokens stay in memory.
export async function connectComputer(context: Context): Promise<void> {
  if (!context.admin) { await (context.loginLocal ?? context.logout)(); return; }
  if (!allowed(context.admin.role, 'admin')) throw new Error('当前账号没有接入电脑的权限。请使用管理员账号登录，或请管理员提供接入凭据。');
  const [nodeData, infoData, preferences] = await Promise.all([
    context.resources.read('nodes', 'nodes', {}, 0), context.host.call('info'), context.host.call('preferences'),
  ]);
  const nodes = records<NodeInfo>(nodeData).filter(node => !disabled(node) && node.online && node.public_host && node.edge_port);
  if (!nodes.length) throw new Error('没有可接入的在线节点。请在“节点管理”检查服务端连接，或使用连接设置手动配置。');
  const defaults = record(record(infoData).local_defaults);
  const previous = record(preferences).local ? record(record(preferences).local) : {};
  const dialog = new FormDialog('连接此电脑', [
    { key: 'name', label: '这台电脑的名称', value: previous.name || defaults.name, required: true, maxLength: 64 },
    { key: 'node', label: '连接到', kind: 'select', value: nodes[0]?.id, options: nodes.map(node => [node.id, node.name]) },
    { ...reason, value: '通过桌面客户端接入此电脑', group: '操作记录' },
  ], async (values, current) => {
    const node = nodes.find(node => node.id === values.node);
    if (!node) throw new Error('请选择服务端节点。');
    const host = node.public_host.includes(':') && !node.public_host.startsWith('[') ? `[${node.public_host}]` : node.public_host;
    await context.host.call('savePreferences', { local: { ...previous, binary: previous.binary || defaults.binary, stateDir: previous.stateDir || defaults.stateDir, name: values.name, controller: `${host}:${node.edge_port}`, tlsPin: node.tls_fingerprint, caFile: '', configFile: '' } });
    const enrollment = record(await context.api.request('createEnrollToken', { body: { kind: 'client', name: values.name, node_id: node.id, ttl_minutes: 5, reason: values.reason } }));
    context.resources.invalidate();
    let token = text(enrollment.token, '');
    try {
      if (!token) throw new Error('管理服务未返回接入凭据，请关闭后刷新核对。');
      await context.host.call('localStart', { enrollToken: token });
    } catch (error) {
      throw new ApiError(0, 'local_start', error instanceof Error ? error.message : '尚未确认接入结果', true);
    } finally { token = ''; enrollment.token = ''; }
    current.finish(); toast('此电脑已接入，接下来可在“隧道管理”添加隧道。'); await context.refresh();
  }, '连接并启动');
  dialog.element.classList.add('connection-dialog');
  const audit = dialog.content.querySelector('fieldset');
  if (audit) dialog.content.append(el('details', 'connection-advanced', el('summary', '', '操作记录'), audit));
  dialog.content.prepend(el('p', 'muted-text', '选择节点后，应用会自动配置连接、获取一次性凭据并启动。无需手动填写地址、证书或程序路径。'));
}
