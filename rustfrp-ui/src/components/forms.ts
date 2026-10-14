import { ApiError } from '../api/client';
import type { Json, RecordValue } from '../api/models';
import { el, button, append } from './dom';

export type FieldKind = 'text' | 'password' | 'number' | 'checkbox' | 'select' | 'textarea' | 'json' | 'list';
export interface Field {
  key: string; label: string; kind?: FieldKind; value?: Json; required?: boolean; min?: number;
  max?: number; maxLength?: number; options?: [string, string][]; description?: string; readonly?: boolean;
  secret?: boolean; maxBytes?: number; group?: string;
}
export const reason: Field = { key: 'reason', label: '操作理由', required: true, maxBytes: 200, description: '该理由与操作结果一起记录到审计日志（最多 200 字节）。' };
export function parseField(field: Field, control: HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement): Json | undefined {
  if (field.kind === 'checkbox' && control instanceof HTMLInputElement) return control.checked;
  const raw = control.value;
  if (field.secret && !raw) return undefined;
  if (field.required && !raw.trim()) throw new Error(`请填写${field.label}`);
  const counted = field.kind === 'password' || field.kind === 'textarea' ? raw : raw.trim();
  if (field.maxLength && Array.from(counted).length > field.maxLength) throw new Error(`${field.label}不能超过 ${field.maxLength} 个字符`);
  if (field.maxBytes && new TextEncoder().encode(raw).length > field.maxBytes) throw new Error(`${field.label}不能超过 ${field.maxBytes} 字节`);
  if (field.kind === 'number') {
    const number = Number(raw);
    if (!raw.trim() || !Number.isSafeInteger(number) || (field.min !== undefined && number < field.min) || (field.max !== undefined && number > field.max)) throw new Error(`${field.label}超出允许的整数范围`);
    return number;
  }
  if (field.kind === 'list') return raw.split(/[\n,]/).map(item => item.trim()).filter(Boolean);
  if (field.kind === 'json') {
    if (!raw.trim()) return null;
    try { return JSON.parse(raw) as Json; } catch { throw new Error(`${field.label}必须是有效的 JSON`); }
  }
  return field.kind === 'password' || field.kind === 'textarea' ? raw : raw.trim();
}
export class FormDialog {
  readonly element = el('dialog', 'dialog');
  readonly form = el('form', 'dialog-form');
  readonly content = el('div', 'dialog-content');
  readonly error = el('p', 'form-error');
  readonly submit: HTMLButtonElement;
  readonly cancel: HTMLButtonElement;
  private readonly controls = new Map<string, HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>();
  private readonly wrappers = new Map<string, HTMLElement>();
  private readonly groups = new Map<string, HTMLFieldSetElement>();
  private readonly restoreFocus: HTMLElement | null;
  private closed = false;
  busy = false;
  dirty = false;
  onClose: () => void = () => {};

  constructor(title: string, readonly fields: Field[], onSubmit: (values: RecordValue, dialog: FormDialog) => Promise<void>, submitLabel = '保存', focus?: HTMLElement | null) {
    this.restoreFocus = focus ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null);
    this.error.setAttribute('role', 'alert'); this.error.hidden = true;
    append(this.form, el('header', 'dialog-heading', el('h2', '', title)), this.content);
    for (const field of fields) this.addField(field);
    this.submit = button(submitLabel, () => {}, { primary: true, id: 'dialog-submit' }); this.submit.type = 'submit';
    this.cancel = button('取消', () => this.close(), { id: 'dialog-cancel' });
    append(this.form, this.error, el('footer', 'dialog-actions', this.cancel, this.submit));
    this.element.append(this.form); document.body.append(this.element);
    this.element.addEventListener('cancel', event => { if (this.busy) event.preventDefault(); });
    this.element.addEventListener('close', () => this.cleanup());
    this.form.addEventListener('input', () => { this.dirty = true; });
    this.form.addEventListener('submit', async event => {
      event.preventDefault(); if (this.busy || this.submit.disabled) return;
      try {
        const values: RecordValue = {};
        for (const field of fields) {
          const control = this.controls.get(field.key);
          const wrapper = this.wrappers.get(field.key);
          if (control && !field.readonly && !wrapper?.hidden && !wrapper?.closest('fieldset')?.hidden) { const parsed = parseField(field, control); if (parsed !== undefined) values[field.key] = parsed; }
        }
        this.setBusy(true); this.error.hidden = true;
        await onSubmit(values, this);
      } catch (error) {
        this.error.textContent = error instanceof Error ? error.message : String(error); this.error.hidden = false;
        if (error instanceof ApiError && error.uncertain) { this.submit.dataset.uncertain = 'true'; this.error.textContent += '。请关闭对话框并刷新核对，不要重复提交。'; }
      } finally { this.setBusy(false); }
    });
    this.element.showModal();
    this.controls.values().next().value?.focus();
  }
  private addField(field: Field): void {
    const id = `field-${field.key}`;
    let control: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
    if (field.kind === 'textarea' || field.kind === 'json' || field.kind === 'list') {
      control = el('textarea'); control.rows = field.kind === 'json' ? 5 : 4;
      control.value = field.kind === 'list' && Array.isArray(field.value) ? field.value.join('\n') : field.kind === 'json' && field.value != null ? JSON.stringify(field.value, null, 2) : String(field.value ?? '');
    } else if (field.kind === 'select') {
      control = el('select'); for (const [value, label] of field.options ?? []) { const option = el('option', '', label); option.value = value; control.append(option); } control.value = String(field.value ?? '');
    } else {
      control = el('input'); control.type = field.kind === 'checkbox' ? 'checkbox' : field.kind === 'number' ? 'number' : field.kind === 'password' ? 'password' : 'text';
      if (field.kind === 'checkbox') control.checked = field.value === true;
      else control.value = field.secret ? '' : String(field.value ?? '');
      if (field.min !== undefined) control.min = String(field.min);
      if (field.max !== undefined) control.max = String(field.max);
      if (field.kind === 'password') control.autocomplete = 'new-password';
    }
    control.id = id; control.name = field.key; control.dataset.testid = id; control.required = !!field.required; control.setAttribute('aria-label', field.label);
    // HTML counts UTF-16 units; validation above counts the same Unicode scalars as Rust.
    if (field.maxLength && !(control instanceof HTMLSelectElement)) control.maxLength = field.maxLength * 2;
    if (field.readonly) { if (control instanceof HTMLSelectElement || (control instanceof HTMLInputElement && control.type === 'checkbox')) control.disabled = true; else control.readOnly = true; }
    this.controls.set(field.key, control);
    const marker = field.required ? el('span', 'required', ' *') : null; marker?.setAttribute('aria-hidden', 'true');
    const label = el('label', '', field.label, marker); label.htmlFor = id;
    const wrapper = el('div', `form-field${field.kind === 'checkbox' ? ' checkbox-field' : ''}`, label, control);
    if (field.description) wrapper.append(el('p', 'field-help', field.description));
    this.wrappers.set(field.key, wrapper);
    if (field.group) {
      let group = this.groups.get(field.group);
      if (!group) { group = el('fieldset', 'form-group', el('legend', '', field.group)); this.groups.set(field.group, group); this.content.append(group); }
      group.append(wrapper);
    } else this.content.append(wrapper);
  }
  control(key: string): HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement | undefined { return this.controls.get(key); }
  set(key: string, value: Json): void {
    const control = this.controls.get(key);
    if (!control) return;
    if (control instanceof HTMLInputElement && control.type === 'checkbox') control.checked = value === true;
    else control.value = Array.isArray(value) ? value.join('\n') : String(value ?? '');
  }
  showField(key: string, visible: boolean): void {
    const wrapper = this.wrappers.get(key); if (wrapper) wrapper.hidden = !visible;
    const control = this.controls.get(key); if (control) control.required = visible && !!this.fields.find(field => field.key === key)?.required;
  }
  showGroup(name: string, visible: boolean): void { const group = this.groups.get(name); if (group) group.hidden = !visible; }
  setBusy(busy: boolean): void {
    this.busy = busy; this.cancel.disabled = busy; this.submit.disabled = busy || this.submit.dataset.uncertain === 'true';
    this.submit.setAttribute('aria-busy', String(busy)); this.element.classList.toggle('busy', busy);
  }
  close(): void { if (this.busy) return; this.element.close(); this.cleanup(); }
  finish(): void { this.busy = false; this.close(); }
  private cleanup(): void {
    if (this.closed) return; this.closed = true;
    for (const control of this.controls.values()) control.value = '';
    this.controls.clear(); this.wrappers.clear(); this.groups.clear(); this.content.replaceChildren(); this.element.remove(); this.restoreFocus?.focus({ preventScroll: true }); this.onClose();
  }
}
export function confirmation(title: string, description: string, action: (reason: string) => Promise<void>): FormDialog {
  const dialog = new FormDialog(title, [reason], async (values, current) => { await action(String(values.reason)); current.finish(); }, '确认');
  dialog.content.prepend(el('p', 'muted-text', description)); return dialog;
}
