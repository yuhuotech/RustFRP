import type { Json, RecordValue } from './models';
import { record } from './models';
import bridge from '../../../rustfrp-protocol/UI_BRIDGE.json';

export const operations = bridge.operations;
export type Operation = keyof typeof operations;
export interface RequestArgs { id?: string; query?: Record<string, string>; body?: RecordValue }
export interface NativeTransport { request(operation: Operation, args: RequestArgs, signal?: AbortSignal): Promise<Json> }

export class ApiError extends Error {
  constructor(readonly status: number, readonly code: string, message: string, readonly uncertain = false) { super(message); }
}
export function requestPath(operation: Operation, args: RequestArgs): string {
  const pattern = operations[operation][1];
  if (pattern.includes('{id}') && (!args.id || args.id.length > 256)) throw new ApiError(0, 'invalid_id', '资源标识无效');
  const path = `/api/v1${pattern.replace('{id}', encodeURIComponent(args.id ?? ''))}`;
  const query = new URLSearchParams(args.query);
  return path + (query.size ? `?${query}` : '');
}
export class ApiClient {
  csrf = '';
  onUnauthorized: () => void = () => {};
  private controllers = new Set<AbortController>();
  constructor(private readonly native?: NativeTransport, private readonly fetcher: typeof fetch = (...args) => fetch(...args), readonly timeout = 15_000) {}

  async request(operation: Operation, args: RequestArgs = {}, signal?: AbortSignal): Promise<Json> {
    const method = operations[operation][0];
    const write = method !== 'GET';
    const mutation = write && operation !== 'previewConfig' && operation !== 'login';
    const abort = new AbortController();
    this.controllers.add(abort);
    const cancel = () => abort.abort(signal?.reason);
    signal?.addEventListener('abort', cancel, { once: true });
    if (signal?.aborted) cancel();
    const timer = setTimeout(() => abort.abort(new Error('timeout')), this.timeout);
    try {
      let value: Json;
      if (this.native) {
        let stop: (() => void) | undefined;
        const cancelled = new Promise<never>((_resolve, reject) => {
          stop = () => reject(new Error('aborted'));
          if (abort.signal.aborted) stop(); else abort.signal.addEventListener('abort', stop, { once: true });
        });
        try { value = await Promise.race([this.native.request(operation, args, abort.signal), cancelled]); }
        finally { if (stop) abort.signal.removeEventListener('abort', stop); }
      }
      else {
        const headers: Record<string, string> = { accept: 'application/json' };
        if (write) { headers['content-type'] = 'application/json'; headers['x-csrf-token'] = this.csrf; }
        if (operation === 'login') headers['x-rustfrp-admin'] = '1';
        const response = await this.fetcher(requestPath(operation, args), {
          method, headers, credentials: 'same-origin', cache: 'no-store', signal: abort.signal,
          body: args.body === undefined ? undefined : JSON.stringify(args.body),
        });
        const body = await response.text();
        try { value = body ? JSON.parse(body) as Json : null; }
        catch { throw new ApiError(response.status, 'invalid_response', response.ok ? '服务器返回了无效的数据，请检查版本是否兼容' : `请求失败（${response.status}），请检查服务器或代理`, mutation && response.status >= 500); }
        if (!response.ok) {
          const error = value && typeof value === 'object' && !Array.isArray(value) ? value.error : null;
          const info = error && typeof error === 'object' && !Array.isArray(error) ? error : {};
          throw new ApiError(response.status, typeof info.code === 'string' ? info.code : `http_${response.status}`, typeof info.message === 'string' ? info.message : `请求失败（${response.status}）`);
        }
      }
      if (operation === 'login' || operation === 'me') {
        const session = record(value);
        // Earlier v1 controllers omit this additive field; explicit incompatible versions fail.
        if (session.api !== undefined && session.api !== bridge.api) throw new ApiError(0, 'incompatible_api', `服务器 API ${String(session.api)} 与界面所需 ${bridge.api} 不兼容，请使用匹配版本`);
        this.csrf = typeof session.csrf === 'string' ? session.csrf : '';
      }
      return value;
    } catch (error) {
      if (error instanceof ApiError) {
        if (error.status === 401 && operation !== 'login') { this.csrf = ''; this.onUnauthorized(); }
        throw error;
      }
      if (signal?.aborted) throw new ApiError(0, 'cancelled', '请求已取消');
      throw new ApiError(0, abort.signal.aborted ? 'timeout' : 'network', mutation ? '操作结果未知，请刷新核对实际状态后再操作' : '无法连接服务器，请检查网络并重试', mutation);
    } finally {
      clearTimeout(timer); signal?.removeEventListener('abort', cancel); this.controllers.delete(abort);
      if (operation === 'logout') this.csrf = '';
    }
  }
  close(): void { for (const controller of this.controllers) controller.abort(); this.controllers.clear(); this.csrf = ''; }
}
