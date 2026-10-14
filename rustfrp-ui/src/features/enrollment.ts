import type { Context } from '../app/context';
import { disabled, record, records, text, date, type EnrollToken, type NodeInfo } from '../api/models';
import { el, button, toast } from '../components/dom';
import { FormDialog, reason, type Field } from '../components/forms';
import { secretDialog } from '../components/operations';

export async function enroll(context: Context, kind: 'client' | 'node' = 'client'): Promise<void> {
  const focus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const nodes = kind === 'client' ? records<NodeInfo>(await context.resources.read('nodes', 'nodes', {}, 4000)).filter(node => !disabled(node) && node.public_host && node.edge_port) : [];
  if (kind === 'client' && !nodes.length) { toast('没有可接入的服务端节点，请先连接一个节点。', 'warning'); return; }
  const fields: Field[] = [
    { key: 'name', label: kind === 'node' ? '节点名称' : '客户端名称', required: true, maxLength: 64 },
    { key: 'ttl_minutes', label: '令牌有效期（分钟）', kind: 'number', value: 60, min: 1, max: 10080 },
    ...(kind === 'client' ? [{ key: 'node_id', label: '所属服务端节点', kind: 'select' as const, value: nodes[0]?.id ?? '', options: nodes.map(node => [node.id, `${node.name} · ${node.online ? '在线' : '离线'}`] as [string, string]) }] : []), reason,
  ];
  new FormDialog(kind === 'node' ? '接入服务端节点' : '接入客户端', fields, async (values, dialog) => {
    const result = record(await context.write('createEnrollToken', { body: { ...values, kind } }));
    dialog.finish(); showEnrollment(context, result, kind);
  }, '创建接入令牌', focus);
}
export function showEnrollment(context: Context, result: ReturnType<typeof record>, kind: 'client' | 'node'): FormDialog {
  const commands = record(result.commands);
  const source = { '接入令牌': text(result.token, ''), ...Object.fromEntries(Object.entries(commands).filter(([, value]) => typeof value === 'string')) };
  const dialog = secretDialog(context, '保存接入凭据', source, `令牌仅在本次显示。有效期至 ${date(result.expires_at)}；请在目标设备执行对应命令。`);
  const state = el('p', 'muted-text', '等待设备使用令牌…'); dialog.content.append(state);
  const open = button('查看已接入资源', () => {}, { disabled: true }); dialog.content.append(open);
  let ended = false; let polling = false; let timer: ReturnType<typeof setInterval> | undefined;
  const close = dialog.onClose;
  dialog.onClose = () => { ended = true; clearInterval(timer); for (const key of Object.keys(source)) source[key as keyof typeof source] = ''; close(); };
  const poll = async () => {
    if (ended || polling || document.hidden) return; polling = true;
    try {
      const tokens = records<EnrollToken>(await context.api.request('enrollTokens'));
      if (ended) return;
      const token = tokens.find(token => token.id === result.id);
      if (token?.used_at != null) {
        clearInterval(timer); state.textContent = '接入成功。凭据已使用，不能再次接入。'; state.className = 'success-text';
        const id = text(token.client_id, ''); open.disabled = !id;
        open.onclick = () => { dialog.close(); context.resources.invalidate(); context.navigate(kind === 'node' ? 'nodes' : 'clients', id); };
      } else if (token?.revoked_at != null || (token && token.expires_at <= Date.now())) {
        clearInterval(timer); state.textContent = token.revoked_at != null ? '令牌已撤销，请重新创建。' : '令牌已过期，请重新创建。';
      }
    } catch (error) { if (!ended) state.textContent = `暂时无法确认接入结果：${error instanceof Error ? error.message : String(error)}。仍可执行接入命令。`; }
    finally { polling = false; }
  };
  timer = setInterval(() => { void poll(); }, 2000); void poll(); return dialog;
}
