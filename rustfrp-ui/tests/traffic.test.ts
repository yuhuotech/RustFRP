import { describe, it, expect } from 'vitest';
import { trafficSlots, trafficChart, slotDescription } from '../src/features/traffic';
import fixture from '../../rustfrp-protocol/fixtures/traffic-history.json';

describe('traffic time and missing-sample semantics', () => {
  it('preserves empty hours and distinguishes sampled zero from missing samples', () => {
    const data = trafficSlots(fixture); expect(data.slots).toHaveLength(24); expect(data.slots[22]).toBeNull(); expect(data.slots[23]?.gap).toBe(true);
    expect(slotDescription(data, 22)).toContain('无采样'); expect(slotDescription(data, 23)).toContain('观测不完整');
    const zero = trafficSlots({ range_hours: 2, bucket_ms: 3600000, window_end: 3600000, items: [{ timestamp: 0, bytes_in: 0, bytes_out: 0, connections_total: 0, gap: false }] });
    expect(slotDescription(zero, 0)).toContain('0 B'); expect(slotDescription(zero, 1)).toContain('无采样');
    const chart = trafficChart(zero); expect(chart.querySelectorAll('rect')).toHaveLength(2); expect(chart.querySelector('rect')?.getAttribute('height')).toBe('2');
  });
  it('places bars by their actual hour and supports keyboard selection', () => {
    const data = trafficSlots(fixture); const chart = trafficChart(data); document.body.append(chart); const svg = chart.querySelector('svg')!;
    expect(chart.querySelector('rect')?.getAttribute('data-hour')).toBe('23');
    svg.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', cancelable: true })); expect(chart.textContent).toContain('无采样');
    for (let i = 0; i < 23; i++) svg.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight' })); expect(chart.textContent).toContain('24 B');
    svg.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft' })); expect(chart.textContent).toContain('无采样');
    svg.dispatchEvent(new MouseEvent('pointerdown', { clientX: 999 })); expect(chart.textContent).toContain('观测不完整');
  });
  it('rejects invalid ranges or measurements instead of rendering invented healthy data', () => {
    expect(() => trafficSlots({ range_hours: 200, bucket_ms: 1, window_end: 1, items: [] })).toThrow('时间范围');
    expect(() => trafficSlots({ ...fixture, items: [{ timestamp: 1, bytes_in: -1, bytes_out: 0 }] })).toThrow('采样值');
  });
});
