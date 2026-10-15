import { text } from '../api/models';

export type Child = Node | string | number | null | undefined | false;
export function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = '', ...children: Child[]): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  if (className) element.className = className;
  append(element, ...children); return element;
}
export function append(parent: Node, ...children: Child[]): void {
  for (const child of children) if (child !== null && child !== undefined && child !== false) parent.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
}
export function keep(parent: Element, children: Node[]): void {
  const wanted = new Set(children);
  for (const child of [...parent.childNodes]) if (!wanted.has(child)) child.remove();
  let at = parent.firstChild;
  for (const child of children) { if (child === at) at = at.nextSibling; else parent.insertBefore(child, at); }
}
const paths: Record<string, string> = {
  overview: 'M3 3h7v7H3z M14 3h7v7h-7z M3 14h7v7H3z M14 14h7v7h-7z',
  nodes: 'M4 3h16v7H4z M4 14h16v7H4z M7 6h.01 M7 17h.01',
  clients: 'M3 3h18v13H3z M8 21h8 M12 16v5',
  tunnels: 'M4 7h7 M13 17h7 M8 7l8 10 M4 3v8 M20 13v8',
  audit: 'M6 3h12v18H6z M9 7h6 M9 11h6 M9 15h4',
  settings: 'M12 3v3 M12 18v3 M3 12h3 M18 12h3 M5.6 5.6l2 2 M16.4 16.4l2 2 M5.6 18.4l2-2 M16.4 7.6l2-2 M8 12a4 4 0 1 0 8 0a4 4 0 1 0-8 0',
  back: 'M15 5l-7 7l7 7', plus: 'M12 5v14 M5 12h14', refresh: 'M20 7v5h-5 M4 17v-5h5 M6 7a7 7 0 0 1 12 0 M18 17a7 7 0 0 1-12 0',
  key: 'M15 4a5 5 0 1 0 0 10a5 5 0 1 0 0-10 M11 12l-8 8 M5 18l2 2',
  activity: 'M2 12h5l3-8l4 16l3-8h5', transfer: 'M5 7h14 M15 3l4 4l-4 4 M19 17H5 M9 13l-4 4l4 4',
  menu: 'M5 7h14 M5 12h14 M5 17h14', copy: 'M9 9h12v12H9z M3 15V3h12',
  'sidebar-close': 'M4 3h16a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1 M9 3v18 M16 9l-3 3l3 3',
  'sidebar-open': 'M4 3h16a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1 M9 3v18 M14 9l3 3l-3 3',
  warning: 'M12 3L2 21h20z M12 9v5 M12 17h.01', local: 'M3 4h18v13H3z M6 8l3 3l-3 3 M12 14h5 M8 21h8',
};
export function icon(name: string): SVGSVGElement {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('fill', 'none'); svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '1.7'); svg.setAttribute('stroke-linecap', 'round'); svg.setAttribute('stroke-linejoin', 'round');
  svg.setAttribute('aria-hidden', 'true'); svg.classList.add('icon');
  const path = document.createElementNS(svg.namespaceURI, 'path'); path.setAttribute('d', paths[name] ?? paths.overview); svg.append(path); return svg;
}
export function button(label: string, action: () => void | Promise<void>, options: { primary?: boolean; danger?: boolean; icon?: string; id?: string; disabled?: boolean | (() => boolean); compact?: boolean } = {}): HTMLButtonElement {
  const element = el('button', `button${options.primary ? ' primary' : ''}${options.danger ? ' danger' : ''}${options.compact ? ' compact' : ''}`, options.icon ? icon(options.icon) : null, label);
  const disabled = () => typeof options.disabled === 'function' ? options.disabled() : !!options.disabled;
  element.type = 'button'; element.disabled = disabled();
  if (options.id) element.dataset.testid = options.id;
  element.addEventListener('click', () => {
    if (element.disabled) return;
    element.focus({ preventScroll: true });
    try {
      const result = action();
      if (result instanceof Promise) {
        element.disabled = true; element.setAttribute('aria-busy', 'true');
        void result.catch(error => toast(error instanceof Error ? error.message : String(error), 'danger')).finally(() => { element.disabled = disabled(); element.setAttribute('aria-busy', 'false'); });
      }
    } catch (error) { toast(error instanceof Error ? error.message : String(error), 'danger'); }
  }); return element;
}
export function badge(label: string, tone = 'muted'): HTMLElement { return el('span', `badge ${tone}`, label); }
function display(content: unknown): string { return typeof content === 'number' && Number.isFinite(content) ? String(content) : typeof content === 'boolean' ? (content ? '是' : '否') : text(content); }
export function value(main: unknown, subtitle?: unknown, tone = ''): HTMLElement {
  const primary = el('span', `cell-main ${tone}`, display(main)); primary.title = display(main);
  const result = el('div', 'cell-text', primary);
  if (subtitle !== undefined) { const second = el('span', 'cell-sub', display(subtitle)); second.title = display(subtitle); result.append(second); }
  return result;
}
export function intro(title: string, description: string, glyph: string, ...badges: Child[]): HTMLElement {
  return el('div', 'page-intro', el('div', `tone-glyph ${glyph}`, icon(glyph)), el('div', 'intro-text', el('div', 'intro-heading', el('h2', '', title), ...badges), el('p', 'muted-text', description)));
}
export function panel(title: string, glyph: string, ...children: Child[]): HTMLElement { return el('section', 'panel', el('h3', 'panel-heading', icon(glyph), title), ...children); }
export function details(fields: [string, unknown][]): HTMLElement {
  const result = el('dl', 'details'); for (const [label, content] of fields) { const v = el('dd', '', display(content)); v.title = display(content); append(result, el('dt', '', label), v); } return result;
}
export function empty(title: string, message = '', action?: HTMLElement): HTMLElement { return el('div', 'empty-state', icon('warning'), el('h3', '', title), el('p', 'muted-text', message), action); }
export function toast(message: string, tone = 'success'): void {
  const root = document.getElementById('notifications');
  if (!root) return;
  const notice = el('div', `toast ${tone}`, message); notice.setAttribute('role', tone === 'danger' ? 'alert' : 'status');
  root.append(notice); while (root.children.length > 4) root.firstElementChild?.remove();
  setTimeout(() => notice.remove(), 6000);
}
export function select(options: [string, string][], selected: string, changed: (value: string) => void, id = ''): HTMLSelectElement {
  const control = el('select'); if (id) { control.dataset.testid = id; control.setAttribute('aria-label', id); }
  for (const [key, label] of options) { const option = el('option', '', label); option.value = key; control.append(option); }
  control.value = selected; control.addEventListener('change', () => changed(control.value)); return control;
}
