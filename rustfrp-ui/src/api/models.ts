export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type RecordValue = { [key: string]: Json };
export type Role = 'viewer' | 'operator' | 'admin' | 'owner';
export const roles: Role[] = ['viewer', 'operator', 'admin', 'owner'];
export const roleNames: Record<Role, string> = { viewer: '只读', operator: '运维', admin: '管理员', owner: '所有者' };

export interface Admin extends RecordValue { id: string; name: string; role: Role; disabled_at: number | null }
export interface Client extends RecordValue {
  id: string; name: string; online: boolean; node_id: string; os: string; arch: string;
  version: string; tags: string[]; last_ip: string; disabled_at: number | null;
  max_connections: number; revision: number; last_seen_at: number | null;
}
export interface NodeInfo extends RecordValue {
  id: string; name: string; builtin: boolean; online: boolean; disabled_at: number | null;
  public_host: string; edge_port: number; tls_fingerprint: string; version: string;
  clients: number; allow_ports: [number, number][]; vhost_http: number | null;
  vhost_https: number | null; tcpmux_http_connect_port: number | null; subdomain_host: string;
}
export interface Tunnel extends RecordValue {
  id: string; name: string; kind: string; client_id: string; enabled: boolean;
  state: string; status_error: string; status_port: number; local_ip: string;
  local_port: number; remote_port: number; custom_domains: string[]; subdomain: string;
}
export interface Visitor extends RecordValue {
  id: string; name: string; client_id: string; tunnel_id: string; bind_addr: string;
  bind_port: number; enabled: boolean; use_encryption: boolean; use_compression: boolean;
}
export interface EnrollToken extends RecordValue {
  id: string; name: string; expires_at: number; used_at: number | null;
  revoked_at: number | null; kind: string; node_id: string;
}
export interface AuditEntry extends RecordValue {
  id: number; ts: number; actor: string; action: string; target_kind: string;
  target_id: string; reason: string; result: string; ip: string;
}
export interface TrafficPoint extends RecordValue {
  timestamp: number; bytes_in: number; bytes_out: number; connections_total: number; gap: boolean;
}
export interface TrafficHistory extends RecordValue {
  range_hours: number; bucket_ms: number; window_end: number; sampled_only: boolean; items: TrafficPoint[];
}

export function record(value: unknown): RecordValue {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('服务器返回了无效的数据结构');
  return value as RecordValue;
}
export function records<T extends RecordValue>(value: unknown): T[] {
  if (!Array.isArray(value)) throw new Error('服务器返回了无效的列表');
  return value.map(item => record(item) as T);
}
export function text(value: unknown, fallback = '—'): string {
  return typeof value === 'string' && value.length > 0 ? value : fallback;
}
export function numeric(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}
export function role(value: unknown): Role { return roles.includes(value as Role) ? value as Role : 'viewer'; }
export function allowed(current: unknown, minimum: Role): boolean { return roles.indexOf(role(current)) >= roles.indexOf(minimum); }
export function disabled(item: RecordValue): boolean { return item.disabled_at !== null && item.disabled_at !== undefined; }
export function status(item: RecordValue): [string, string] {
  if (disabled(item)) return ['已停用', 'muted'];
  if ('online' in item) return item.online ? ['在线', 'success'] : ['离线', 'muted'];
  const states: Record<string, [string, string]> = { running: ['运行中', 'success'], error: ['异常', 'danger'], pending: ['待启动', 'warning'], stopped: ['已停止', 'muted'] };
  return states[text(item.state, '')] ?? ['未知', 'muted'];
}
export function tokenStatus(item: RecordValue, now = Date.now()): [string, string] {
  if (item.revoked_at != null) return ['已撤销', 'muted'];
  if (item.used_at != null) return ['已使用', 'success'];
  const expires = numeric(item.expires_at);
  if (expires === null) return ['未知', 'muted'];
  return expires <= now ? ['已过期', 'muted'] : ['待使用', 'warning'];
}
export function bytes(value: unknown): string {
  const n = numeric(value);
  if (n === null || n < 0) return '—';
  if (n < 1024) return `${n} B`;
  const exponent = Math.min(Math.floor(Math.log(n) / Math.log(1024)), 4);
  return `${(n / 1024 ** exponent).toFixed(1)} ${['B', 'KiB', 'MiB', 'GiB', 'TiB'][exponent]}`;
}
export function date(value: unknown): string {
  const n = numeric(value);
  return n === null || n <= 0 ? '—' : new Date(n).toLocaleString('zh-CN', { hour12: false });
}
export function address(host: unknown, port: unknown): string {
  const name = text(host, '');
  const n = numeric(port);
  if (!name || !n) return '未分配';
  return `${name.includes(':') && !name.startsWith('[') ? `[${name}]` : name}:${n}`;
}
export function publicAddress(tunnel: Tunnel, node?: NodeInfo): string {
  if (tunnel.kind === 'stcp' || tunnel.kind === 'sudp') return '私密隧道，通过访问者接入';
  const host = tunnel.custom_domains?.[0] || (tunnel.subdomain && node?.subdomain_host ? `${tunnel.subdomain}.${node.subdomain_host}` : node?.public_host);
  if (tunnel.kind === 'http' || tunnel.kind === 'https') {
    if (!host) return '未配置域名';
    const port = tunnel.kind === 'http' ? node?.vhost_http : node?.vhost_https;
    return `${tunnel.kind}://${host}${port && port !== (tunnel.kind === 'http' ? 80 : 443) ? `:${port}` : ''}`;
  }
  if (tunnel.kind === 'tcpmux') return address(node?.public_host, node?.tcpmux_http_connect_port);
  return address(host, tunnel.status_port || tunnel.remote_port);
}
