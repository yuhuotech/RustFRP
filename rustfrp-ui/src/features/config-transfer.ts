import type { Context } from '../app/context';
import { record, records, text, type RecordValue } from '../api/models';
import { ApiError } from '../api/client';
import { el, button, badge, toast } from '../components/dom';
import { FormDialog, reason } from '../components/forms';
import { secretDialog } from '../components/operations';
import { ResourceTable } from '../components/table';

export function configImport(context: Context, id: string): FormDialog {
  let preview: RecordValue | undefined; let previewText = ''; let reading = false; let closed = false; let abort: AbortController | undefined;
  const result = el('div'); const table = new ResourceTable(['变更', '隧道 / 类型', '本地目标', '远程端口', '状态'], 640);
  const dialog = new FormDialog('导入 frpc 配置', [
    { key: 'text', label: 'TOML / YAML / JSON 完整配置', kind: 'textarea', required: true, maxBytes: 1024 * 1024, description: '只增改导入中出现的同名隧道，保留其他隧道。最多 100 条代理；禁止 includes、tokenSource 和环境模板。' },
    { key: 'accept_warnings', label: '已检查兼容报告，并确认按上述变更导入', kind: 'checkbox', value: false }, reason,
  ], async (values, current) => {
    if (reading || !preview || previewText !== values.text || typeof preview.digest !== 'string' || values.accept_warnings !== true) throw new Error('请先预览当前配置，再确认兼容报告与变更。');
    await context.write('importConfig', { id, body: { text: values.text, digest: preview.digest, accept_warnings: true, reason: values.reason } });
    current.finish(); toast('配置已原子导入，已通知客户端刷新。');
  }, '确认导入');
  const input = dialog.control('text'); const acknowledgment = dialog.control('accept_warnings');
  const refreshEligibility = () => { dialog.submit.disabled = dialog.busy || reading || !preview || previewText !== input?.value || !(acknowledgment instanceof HTMLInputElement && acknowledgment.checked) || dialog.submit.dataset.uncertain === 'true'; };
  const previewButton = button('预览变更', async () => {
    if (reading || dialog.busy) return;
    reading = true; previewButton.disabled = true; previewButton.setAttribute('aria-busy', 'true'); preview = undefined; dialog.set('accept_warnings', false); dialog.error.hidden = true;
    const source = input?.value ?? ''; abort = new AbortController();
    try {
      if (!source.trim() || new TextEncoder().encode(source).length > 1024 * 1024) throw new Error('请填写不超过 1 MiB 的配置文本。');
      const data = record(await context.api.request('previewConfig', { id, body: { text: source } }, abort.signal));
      if (closed || input?.value !== source) return;
      preview = data; previewText = source;
      const warnings = Array.isArray(data.warnings) ? data.warnings.filter(item => typeof item === 'string') : [];
      const changes = records<RecordValue>(data.changes);
      table.update(changes.map((item, index) => ({ id: String(index), signature: JSON.stringify(item), cells: () => [el('span', '', item.action === 'create' ? '新增' : item.action === 'update' ? '修改' : String(item.action)), el('span', '', `${item.name} · ${item.kind}`), el('span', '', `${item.local_ip}:${item.local_port}`), el('span', '', String(item.remote_port)), badge(item.enabled ? '启用' : '停用', item.enabled ? 'success' : 'muted')] })));
      result.replaceChildren(el('h3', '', `预览 ${changes.length} 条变更`), ...warnings.map(warning => el('p', 'warning-text', warning)), ...(warnings.length ? [] : [el('p', 'muted-text', '未发现兼容性警告。')]), table.element);
      dialog.set('accept_warnings', warnings.length === 0);
    } catch (error) { if (!closed && !(error instanceof ApiError && error.code === 'cancelled')) { dialog.error.textContent = error instanceof Error ? error.message : String(error); dialog.error.hidden = false; } }
    finally { reading = false; previewButton.disabled = false; previewButton.setAttribute('aria-busy', 'false'); refreshEligibility(); }
  }, { id: 'config-preview' });
  const before = acknowledgment?.closest('.form-field');
  if (before) { dialog.content.insertBefore(previewButton, before); dialog.content.insertBefore(result, before); }
  else { dialog.content.append(previewButton, result); }
  input?.addEventListener('input', () => { preview = undefined; dialog.set('accept_warnings', false); result.replaceChildren(el('p', 'muted-text', '配置已修改，请重新预览。')); refreshEligibility(); });
  acknowledgment?.addEventListener('change', refreshEligibility);
  dialog.onClose = () => { closed = true; abort?.abort(); previewText = ''; preview = undefined; result.replaceChildren(); table.dispose(); };
  refreshEligibility(); return dialog;
}
export async function configExport(context: Context, id: string): Promise<void> {
  const result = record(await context.api.request('exportConfig', { id }));
  secretDialog(context, '导出标准 frpc 配置', { 'frpc.toml': text(result.text, '') }, '导出包含 HTTP Basic Auth 密码，请安全保存。设备身份与服务端接入凭据已省略；关闭后不会缓存配置原文。');
}
