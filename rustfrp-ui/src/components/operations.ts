import type { Context } from '../app/context';
import type { Json, RecordValue } from '../api/models';
import { FormDialog, reason, type Field } from './forms';
import { button, el, toast } from './dom';

export function edit(title: string, fields: Field[], save: (values: RecordValue) => Promise<unknown>, label = '保存', focus?: HTMLElement | null): FormDialog {
  return new FormDialog(title, [...fields, { ...reason, group: '操作确认' }], async (values, dialog) => {
    await save(values); dialog.finish(); toast(`${title}成功`);
  }, label, focus);
}
export function copy(context: Context, source: () => string, label = '复制'): HTMLButtonElement {
  return button(label, async () => {
    try { await context.host.call('copy', { text: source() }); toast('已复制'); }
    catch (error) { context.showError(error); }
  }, { icon: 'copy' });
}
export function secretDialog(context: Context, title: string, source: RecordValue, notice: string): FormDialog {
  const dialog = new FormDialog(title, [], async (_, current) => current.finish(), '完成');
  dialog.cancel.hidden = true;
  dialog.content.append(el('p', 'warning-text', notice));
  const fields: HTMLTextAreaElement[] = [];
  for (const [label, value] of Object.entries(source)) {
    let content = typeof value === 'string' ? value : JSON.stringify(value, null, 2);
    const field = el('textarea', 'copy-field'); field.readOnly = true; field.value = content; field.rows = Math.min(8, content.split('\n').length + 1); field.setAttribute('aria-label', label);
    dialog.content.append(el('h3', '', label), field, copy(context, () => field.value));
    fields.push(field);
    content = '';
  }
  dialog.element.dataset.sensitive = 'true';
  dialog.onClose = () => { for (const field of fields) field.value = ''; fields.length = 0; for (const key of Object.keys(source)) source[key] = ''; };
  return dialog;
}
export function guarded(context: Context, task: () => unknown | Promise<unknown>): () => Promise<void> {
  let pending = false;
  return async () => {
    if (pending) return; pending = true;
    try { await task(); } catch (error) { context.showError(error); }
    finally { pending = false; }
  };
}
export class View {
  private signature = '';
  constructor(readonly element: HTMLElement) {}
  update(data: Json, render: () => HTMLElement[]): void {
    const signature = JSON.stringify(data); if (signature === this.signature) return;
    const focus = this.element.contains(document.activeElement) && document.activeElement instanceof HTMLElement ? document.activeElement.dataset.testid : undefined;
    this.element.replaceChildren(...render()); this.signature = signature;
    if (focus) this.element.querySelector<HTMLElement>(`[data-testid="${CSS.escape(focus)}"]`)?.focus({ preventScroll: true });
  }
}
