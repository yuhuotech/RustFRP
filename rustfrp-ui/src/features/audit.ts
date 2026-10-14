import type { Context, Page } from '../app/context';
import { date, records, type AuditEntry } from '../api/models';
import { badge, button, el, intro, value } from '../components/dom';
import { guarded } from '../components/operations';
import { ResourceTable } from '../components/table';

const actionLabels: Record<string, string> = {
  'auth.login': '登录', 'client.enroll': '客户端接入', 'client.update': '修改客户端', 'client.kick': '强制下线', 'client.delete': '删除客户端',
  'client.move_node': '迁移服务端节点', 'client.restart': '重启客户端', 'client.update_agent': '下发 Agent 更新',
  'tunnel.create': '创建隧道', 'tunnel.update': '修改隧道', 'tunnel.delete': '删除隧道', 'tunnel.batch': '批量调整隧道',
  'visitor.create': '创建访问者', 'visitor.update': '修改访问者', 'visitor.delete': '删除访问者',
  'enroll_token.create': '创建接入令牌', 'enroll_token.revoke': '撤销接入令牌', 'node_token.create': '创建节点接入令牌',
  'node.enroll': '服务端节点接入', 'node.update': '修改服务端节点', 'node.delete': '删除服务端节点',
  'admin.create': '新增管理员', 'admin.update': '修改管理员', 'admin.passwd': '重置管理员密码', 'permission.denied': '权限不足',
  'config.import': '导入配置', 'api_token.create': '创建 API Token', 'api_token.revoke': '撤销 API Token',
};
export function actionLabel(action: string): string { return actionLabels[action] ?? action; }
export function auditPage(context: Context): Page {
  const element = el('div', 'page-content'); const count = el('p', 'muted-text'); const table = new ResourceTable(['时间', '操作 / 原始标识', '操作人 / 来源 IP', '目标资源', '理由', '结果'], 1040);
  let items: AuditEntry[] = []; let disposed = false; let reading = false; let exhausted = false;
  const abort = new AbortController();
  const older = button('加载更早记录', guarded(context, async () => {
    if (reading || exhausted || items.length >= 1000) return;
    reading = true;
    try {
      const values = records<AuditEntry>(await context.api.request('audit', { query: { limit: '100', before: String(items.at(-1)?.id ?? 0) } }, abort.signal));
      if (!disposed) { exhausted = values.length < 100; items = [...items, ...values].slice(0, 1000); render(); }
    } finally { reading = false; }
  }), { id: 'audit-older', disabled: () => reading || exhausted || items.length >= 1000 || items.length === 0 });
  element.append(intro('操作追踪', '核对操作人、来源、目标和理由。未知操作保留原始标识。', 'audit'), count, table.element, el('div', 'filters', older));
  function render(): void {
    count.textContent = `本次加载 ${items.length} 条（最多保留 1000 条，刷新从最新记录开始）。`;
    older.disabled = exhausted || items.length >= 1000 || items.length === 0;
    table.update(items.map(item => ({ id: String(item.id), signature: JSON.stringify(item), cells: () => [value(date(item.ts)), value(actionLabel(item.action), item.action, 'blue'), value(item.actor, item.ip || '无来源 IP'), value(item.target_id || '—', item.target_kind), value(item.reason || '未填写'), badge(item.result === 'ok' ? '成功' : item.result === 'denied' ? '拒绝' : item.result, item.result === 'ok' ? 'success' : 'danger')] })));
  }
  async function refresh(force = false): Promise<void> {
    if (reading) return; reading = true;
    try {
      const latest = records<AuditEntry>(await context.resources.read('audit-latest', 'audit', { query: { limit: '100' } }, force ? 0 : 4000));
      if (disposed) return;
      if (force || !items.length) { items = latest; exhausted = latest.length < 100; }
      else { const seen = new Set(latest.map(item => item.id)); items = [...latest, ...items.filter(item => !seen.has(item.id))].slice(0, 1000); }
      render();
    } finally { reading = false; older.disabled = exhausted || items.length >= 1000 || items.length === 0; }
  }
  return { element, actions: [], refresh, dispose() { disposed = true; abort.abort(); items = []; table.dispose(); } };
}
