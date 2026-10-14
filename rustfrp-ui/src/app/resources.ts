import type { Json } from '../api/models';
import type { ApiClient, Operation, RequestArgs } from '../api/client';

interface Entry { value?: Json; at: number; bytes: number; flight?: Promise<Json>; abort?: AbortController }
export class Resources {
  private entries = new Map<string, Entry>();
  private listeners = new Set<() => void>();
  private revision = 0;
  constructor(private readonly api: ApiClient, readonly limit = 32, private readonly clock = () => Date.now(), readonly byteLimit = 16 * 1024 * 1024) {}
  subscribe(listener: () => void): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
  private emit(): void { for (const listener of this.listeners) listener(); }
  get(key: string): Json | undefined { return this.entries.get(key)?.value; }
  async read(key: string, operation: Operation, args: RequestArgs = {}, maxAge = 4000): Promise<Json> {
    let entry = this.entries.get(key);
    if (entry?.flight) return entry.flight;
    if (entry?.value !== undefined && this.clock() - entry.at < maxAge) return entry.value;
    if (!entry) {
      if (this.entries.size >= this.limit) { const oldest = this.entries.keys().next().value; if (oldest !== undefined) { this.entries.get(oldest)?.abort?.abort(); this.entries.delete(oldest); } }
      entry = { at: 0, bytes: 0 }; this.entries.set(key, entry);
    }
    const item = entry;
    const generation = this.revision;
    const abort = new AbortController(); item.abort = abort;
    const flight = this.api.request(operation, args, abort.signal).then(value => {
      if (this.revision === generation && this.entries.get(key) === item) {
        const changed = JSON.stringify(item.value) !== JSON.stringify(value);
        // A conservative serialized-size multiplier also bounds object overhead.
        item.bytes = JSON.stringify(value).length * 4;
        item.value = value; item.at = this.clock();
        let size = [...this.entries.values()].reduce((total, entry) => total + entry.bytes, 0);
        for (const [oldest, entry] of this.entries) {
          if (size <= this.byteLimit) break;
          size -= entry.bytes; entry.abort?.abort(); this.entries.delete(oldest);
        }
        if (changed) this.emit();
      }
      return value;
    }).finally(() => { if (item.flight === flight) { item.flight = undefined; item.abort = undefined; } });
    item.flight = flight;
    return flight;
  }
  invalidate(): void {
    this.revision++;
    for (const item of this.entries.values()) { item.at = -Infinity; item.abort?.abort(); item.flight = undefined; }
  }
  cancel(): void { for (const item of this.entries.values()) item.abort?.abort(); }
  clear(): void { this.invalidate(); this.entries.clear(); this.emit(); }
  dispose(): void { this.clear(); this.listeners.clear(); }
}
