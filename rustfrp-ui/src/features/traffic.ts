import { bytes, numeric, record, records, type RecordValue } from '../api/models';
import { el } from '../components/dom';

export interface TrafficSlots { hours: number; end: number; start: number; bucket: number; slots: (RecordValue | null)[] }
export function trafficSlots(value: unknown): TrafficSlots {
  const history = record(value);
  const hours = numeric(history.range_hours); const end = numeric(history.window_end); const bucket = numeric(history.bucket_ms);
  if (hours === null || !Number.isInteger(hours) || hours < 1 || hours > 168 || end === null || bucket === null || bucket <= 0) throw new Error('流量采样时间范围无效');
  const start = end - (hours - 1) * bucket; const slots: (RecordValue | null)[] = Array.from({ length: hours }, () => null);
  for (const point of records(history.items)) {
    const timestamp = numeric(point.timestamp); const incoming = numeric(point.bytes_in); const outgoing = numeric(point.bytes_out);
    if (timestamp === null || incoming === null || outgoing === null || incoming < 0 || outgoing < 0) throw new Error('流量采样值无效');
    const index = Math.floor((timestamp - start) / bucket);
    if (index >= 0 && index < hours) slots[index] = point;
  }
  return { hours, end, start, bucket, slots };
}
export function slotDescription(data: TrafficSlots, index: number): string {
  const time = new Date(data.start + index * data.bucket).toLocaleString('zh-CN', { hour12: false });
  const point = data.slots[index];
  return `${time} · ${point ? `入站 ${bytes(point.bytes_in)} / 出站 ${bytes(point.bytes_out)} · 新建连接 ${numeric(point.connections_total) ?? '—'}${point.gap ? ' · 观测不完整' : ''}` : '无采样'}`;
}
export function trafficChart(data: TrafficSlots): HTMLElement {
  const root = el('div', 'traffic-chart');
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); svg.classList.add('chart');
  svg.setAttribute('viewBox', '0 0 1000 160'); svg.setAttribute('role', 'img'); svg.setAttribute('tabindex', '0'); svg.setAttribute('aria-label', '入站与出站小时流量图，方向键或点击查看小时数据'); svg.dataset.testid = 'traffic-chart';
  const peak = Math.max(1, ...data.slots.map(point => point ? Math.max(Number(point.bytes_in), Number(point.bytes_out)) : 0));
  const step = 1000 / data.hours;
  for (const [index, point] of data.slots.entries()) {
    if (!point) continue;
    for (const [direction, color, offset] of [['bytes_in', 'var(--action)', .12], ['bytes_out', 'var(--blue)', .54]] as const) {
      const bar = document.createElementNS(svg.namespaceURI, 'rect'); const height = Math.max(2, Number(point[direction]) / peak * 140);
      bar.setAttribute('x', String(index * step + offset * step)); bar.setAttribute('y', String(160 - height));
      bar.setAttribute('width', String(Math.max(.5, step * .32))); bar.setAttribute('height', String(height)); bar.setAttribute('fill', color); bar.setAttribute('data-hour', String(index));
      const tip = document.createElementNS(svg.namespaceURI, 'title'); tip.textContent = slotDescription(data, index); bar.append(tip); svg.append(bar);
    }
    if (point.gap) {
      const marker = document.createElementNS(svg.namespaceURI, 'line'); marker.setAttribute('x1', String((index + .5) * step)); marker.setAttribute('x2', String((index + .5) * step)); marker.setAttribute('y1', '0'); marker.setAttribute('y2', '160'); marker.setAttribute('stroke', 'var(--border)'); marker.setAttribute('stroke-dasharray', '3 5'); svg.append(marker);
    }
  }
  const label = el('p', 'chart-note', '入站：公网 → 本机 · 出站：本机 → 公网'); label.dataset.testid = 'traffic-detail'; label.setAttribute('aria-live', 'polite');
  let selected = -1;
  const choose = (index: number) => { selected = Math.max(0, Math.min(data.hours - 1, index)); label.textContent = slotDescription(data, selected); };
  const pointer = (event: PointerEvent) => { const rect = svg.getBoundingClientRect(); choose(Math.floor((event.clientX - rect.left) / (rect.width || 1000) * data.hours)); };
  svg.addEventListener('pointermove', pointer); svg.addEventListener('pointerdown', pointer);
  svg.addEventListener('keydown', event => { if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); choose(selected < 0 ? 0 : selected + (event.key === 'ArrowRight' ? 1 : -1)); } });
  root.append(svg, el('div', 'chart-labels', el('span', '', new Date(data.start).toLocaleString('zh-CN')), el('span', '', new Date(data.end).toLocaleString('zh-CN'))), label);
  return root;
}
