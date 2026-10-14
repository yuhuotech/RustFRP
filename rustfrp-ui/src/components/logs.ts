import { record, type Json } from '../api/models';
import { el } from './dom';

export class LogView {
  readonly element = el('div', 'log-view');
  readonly notice = el('p', 'muted-text');
  cursor = 0;
  private lines: string[] = [];
  constructor(readonly limit = 1024) { this.element.setAttribute('role', 'log'); this.element.setAttribute('aria-label', '日志记录'); }
  append(value: Json): void {
    const response = record(value);
    if (!Array.isArray(response.items) || response.items.some(item => typeof item !== 'string') || typeof response.cursor !== 'number' || !Number.isSafeInteger(response.cursor) || response.cursor < 0) throw new Error('日志响应格式无效');
    const atBottom = this.element.scrollHeight - this.element.scrollTop - this.element.clientHeight < 40;
    if (response.gap) this.lines.push('—— 部分日志已被环形缓冲覆盖，请检查设备状态。——');
    const added = response.items as string[];
    this.lines.push(...added.map(line => line.slice(0, 1024))); this.lines = this.lines.slice(-this.limit); this.cursor = response.cursor;
    if (!this.lines.length) this.element.replaceChildren(el('p', 'muted-text', '暂无日志'));
    else if (added.length || response.gap) this.element.replaceChildren(...this.lines.map(line => el('div', 'log-line', line)));
    if (atBottom) this.element.scrollTop = this.element.scrollHeight;
    this.notice.textContent = `本次累计保留 ${this.lines.length} 条，显示上限 ${this.limit} 条；服务端环形容量 ${response.capacity ?? '—'} 条。`;
  }
  snapshot(value: Json): void {
    const response = record(value);
    if (!Array.isArray(response.items) || response.items.length > 128 || response.items.some(item => typeof item !== 'string' || new TextEncoder().encode(item).length > 512)) throw new Error('本机日志快照格式无效');
    const lines = response.items as string[];
    if (JSON.stringify(lines) !== JSON.stringify(this.lines)) { this.lines = lines.slice(-this.limit); this.element.replaceChildren(...this.lines.map(line => el('div', 'log-line', line))); }
    if (!this.lines.length) this.element.replaceChildren(el('p', 'muted-text', '暂无日志'));
    this.notice.textContent = `当前 Agent 提供日志快照，共 ${this.lines.length} 条。升级 Agent 后可使用增量日志与丢失提示。`;
  }
  clear(): void { this.cursor = 0; this.lines = []; this.element.replaceChildren(); this.notice.textContent = ''; }
}
