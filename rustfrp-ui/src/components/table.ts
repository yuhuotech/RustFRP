import { el, keep, empty } from './dom';

export interface Row { id: string; signature: string; cells: () => HTMLElement[] }
export class ResourceTable {
  readonly element = el('div', 'table-scroll');
  private table = el('table', 'resource-table');
  private body = el('tbody');
  private rows: Row[] = [];
  private nodes = new Map<string, { signature: string; element: HTMLTableRowElement }>();
  private frame = 0;
  private disposed = false;
  private virtual = false;
  readonly rowHeight = 80;
  constructor(readonly columns: string[], minWidth = 880) {
    this.table.style.minWidth = `${minWidth}px`;
    const header = el('tr'); for (const name of columns) { const th = el('th', '', name); th.scope = 'col'; header.append(th); }
    this.table.append(el('thead', '', header), this.body); this.element.append(this.table);
    this.element.addEventListener('scroll', () => {
      if (this.virtual && !this.frame) this.frame = requestAnimationFrame(() => { this.frame = 0; if (!this.disposed) this.render(); });
    });
  }
  update(rows: Row[]): void { this.rows = rows; this.virtual = rows.length > 200; this.element.classList.toggle('virtual', this.virtual); this.render(); }
  private spacer(height: number): HTMLTableRowElement {
    const row = el('tr', 'table-spacer'); row.setAttribute('aria-hidden', 'true');
    const cell = el('td'); cell.colSpan = this.columns.length; cell.style.height = `${height}px`; row.append(cell); return row;
  }
  private render(): void {
    if (!this.rows.length) { this.body.replaceChildren(); this.nodes.clear(); this.table.hidden = true; if (!this.element.querySelector('.empty-state')) this.element.append(empty('暂无记录', '接入资源或调整筛选后查看。')); return; }
    this.table.hidden = false; this.element.querySelector('.empty-state')?.remove();
    const count = this.virtual ? Math.ceil((this.element.clientHeight || 600) / this.rowHeight) + 8 : this.rows.length;
    const start = this.virtual ? Math.min(Math.max(0, this.rows.length - count), Math.max(0, Math.floor((this.element.scrollTop - 40) / this.rowHeight) - 4)) : 0;
    const active = this.rows.slice(start, start + count);
    const wanted = new Set(active.map(row => row.id));
    for (const id of this.nodes.keys()) if (!wanted.has(id)) this.nodes.delete(id);
    const nodes: Node[] = [];
    if (start > 0) nodes.push(this.spacer(start * this.rowHeight));
    for (const row of active) {
      let cached = this.nodes.get(row.id);
      if (!cached) { cached = { signature: '', element: el('tr') }; cached.element.dataset.rowId = row.id; this.nodes.set(row.id, cached); }
      if (cached.signature !== row.signature) {
        const focused = cached.element.contains(document.activeElement) && document.activeElement instanceof HTMLElement ? document.activeElement.dataset.testid : undefined;
        cached.element.replaceChildren(...row.cells().map((content, index) => { const cell = el('td', '', content); cell.dataset.label = this.columns[index]; return cell; }));
        cached.signature = row.signature;
        if (focused) cached.element.querySelector<HTMLElement>(`[data-testid="${CSS.escape(focused)}"]`)?.focus({ preventScroll: true });
      }
      nodes.push(cached.element);
    }
    const end = start + active.length;
    if (end < this.rows.length) nodes.push(this.spacer((this.rows.length - end) * this.rowHeight));
    keep(this.body, nodes);
  }
  dispose(): void { this.disposed = true; cancelAnimationFrame(this.frame); this.nodes.clear(); }
}
