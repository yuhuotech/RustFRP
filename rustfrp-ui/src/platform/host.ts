import { invoke, isTauri } from '@tauri-apps/api/core';
import type { Json, RecordValue } from '../api/models';
import type { NativeTransport, Operation, RequestArgs } from '../api/client';
import { ApiError } from '../api/client';

export type HostAction = 'info' | 'preferences' | 'savePreferences' | 'selectController' | 'localStatus' | 'localStart' | 'localStop' | 'localLogs' | 'localTunnels' | 'readConfig' | 'verifyConfig' | 'saveConfig' | 'exportDiagnostics' | 'startup' | 'setStartup' | 'notifications' | 'setNotifications' | 'updateStatus' | 'checkUpdate' | 'installUpdate' | 'cancelUpdate' | 'window' | 'windowState' | 'pickFile' | 'openUrl' | 'copy';
export interface Host {
  desktop: boolean;
  call(action: HostAction, args?: RecordValue): Promise<Json>;
  request(operation: Operation, args: RequestArgs, signal?: AbortSignal): Promise<Json>;
}
export class DesktopHost implements Host, NativeTransport {
  readonly desktop = true;
  constructor(private readonly dispatch: typeof invoke = invoke) {}
  async call(action: HostAction, args: RecordValue = {}): Promise<Json> {
    try { return await this.dispatch<Json>('host_command', { action, args }); }
    catch (error) { throw nativeError(error); }
  }
  async request(operation: Operation, args: RequestArgs, signal?: AbortSignal): Promise<Json> {
    const requestId = crypto.randomUUID();
    const cancel = () => { void this.dispatch('controller_cancel', { requestId }).catch(() => {}); };
    signal?.addEventListener('abort', cancel, { once: true });
    if (signal?.aborted) { signal.removeEventListener('abort', cancel); throw new Error('aborted'); }
    try { return await this.dispatch<Json>('controller_request', { operation, args, requestId }); }
    catch (error) {
      throw nativeError(error);
    } finally { signal?.removeEventListener('abort', cancel); }
  }
}
export function nativeError(error: unknown): Error {
  if (error instanceof Error) return error;
  if (error && typeof error === 'object' && 'status' in error && 'message' in error) {
    const info = error as { status: number; code?: string; message: string; uncertain?: boolean };
    return new ApiError(info.status, info.code ?? 'native', info.message, !!info.uncertain);
  }
  return new Error(typeof error === 'string' ? error : '桌面操作失败，请刷新状态后重试');
}
export class WebHost implements Host {
  readonly desktop = false;
  async call(action: HostAction, args: RecordValue = {}): Promise<Json> {
    if (action === 'copy') { await navigator.clipboard.writeText(String(args.text ?? '')); return null; }
    if (action === 'openUrl') {
      const url = new URL(String(args.url));
      if (!['http:', 'https:'].includes(url.protocol)) throw new Error('不支持的链接');
      window.open(url.href, '_blank', 'noopener,noreferrer'); return null;
    }
    if (action === 'info') return { desktop: false, version: document.querySelector<HTMLMetaElement>('meta[name="rustfrp-ui-version"]')?.content || 'development' };
    throw new Error('此操作需要桌面客户端');
  }
  async request(): Promise<Json> { throw new Error('浏览器请求使用同源 HTTP'); }
}
export function createHost(): Host { return isTauri() ? new DesktopHost() : new WebHost(); }
