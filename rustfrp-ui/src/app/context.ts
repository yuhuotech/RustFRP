import type { ApiClient } from '../api/client';
import type { Admin, Json } from '../api/models';
import type { Host } from '../platform/host';
import type { Resources } from './resources';

export type Section = 'overview' | 'nodes' | 'clients' | 'tunnels' | 'audit' | 'settings' | 'local';
export interface Context {
  api: ApiClient; resources: Resources; host: Host; admin: Admin | null;
  navigate(section: Section, id?: string): void;
  refresh(): Promise<void>;
  logout(): Promise<void>;
  showError(error: unknown): void;
  write(operation: Parameters<ApiClient['request']>[0], args: Parameters<ApiClient['request']>[1]): Promise<Json>;
}
export interface Page { element: HTMLElement; actions: HTMLElement[]; refresh(force?: boolean): Promise<void>; dispose(): void }
