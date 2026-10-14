import { ApiClient } from '../api/client';
import { allowed, record, role, type Admin } from '../api/models';
import { createHost, type Host } from '../platform/host';
import { Resources } from './resources';
import type { Context, Page, Section } from './context';
import { el, button, icon, toast, empty } from '../components/dom';
import { loadPage } from '../features/resource-pages';
import { observeWindow } from '../platform/window';

const titles: Record<Section, string> = { overview: '概览', nodes: '节点管理', clients: '客户端', tunnels: '隧道管理', audit: '审计日志', settings: '系统设置', local: '本机模式' };
export class App {
  readonly api: ApiClient;
  readonly resources: Resources;
  private admin: Admin | null = null;
  private page?: Page;
  private content?: HTMLElement;
  private actions?: HTMLElement;
  private section: Section = 'overview';
  private id?: string;
  private timer?: ReturnType<typeof setInterval>;
  private busy = false;
  private disposed = false;
  private generation = 0;
  private stopWindow: () => void = () => {};
  private readonly visibility = () => { if (!document.hidden) void this.refresh(); };
  constructor(readonly root: HTMLElement, readonly host: Host = createHost(), api?: ApiClient) {
    this.api = api ?? new ApiClient(host.desktop ? host : undefined); this.resources = new Resources(this.api);
    this.api.onUnauthorized = () => { this.admin = null; this.resources.clear(); this.login(); };
    this.applyTheme();
  }
  private applyTheme(): void {
    const theme = localStorage.getItem('rustfrp.theme') ?? 'system';
    document.documentElement.dataset.theme = theme === 'system' ? (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light') : theme;
    document.documentElement.dataset.collapsed = String(localStorage.getItem('rustfrp.collapsed') === 'true');
  }
  async start(): Promise<void> {
    this.root.replaceChildren(empty('正在连接', '正在恢复会话…'));
    document.addEventListener('visibilitychange', this.visibility);
    this.stopWindow = observeWindow(this.host);
    try {
      if (this.host.desktop) {
        const preferences = record(await this.host.call('preferences'));
        if (typeof preferences.theme === 'string') { localStorage.setItem('rustfrp.theme', preferences.theme); this.applyTheme(); }
        if (typeof preferences.sidebar_collapsed === 'boolean') { localStorage.setItem('rustfrp.collapsed', String(preferences.sidebar_collapsed)); this.applyTheme(); }
        if (typeof preferences.controller_url === 'string' && preferences.controller_url) await this.host.call('selectController', { url: preferences.controller_url });
        if (preferences.local_mode === true) { this.section = 'local'; this.restorePage(); this.shell(); return; }
      }
      const value = record(await this.api.request('me')); this.admin = { ...record(value.admin), role: role(record(value.admin).role) } as Admin;
      if (!this.disposed) { this.restorePage(); this.shell(); }
    } catch (error) { if (!this.disposed) this.login(error); }
  }
  private context(): Context {
    return { api: this.api, resources: this.resources, host: this.host, admin: this.admin,
      navigate: (section, id) => this.navigate(section, id), refresh: () => this.refresh(true), logout: () => this.logout(), showError: error => this.showError(error),
      write: async (operation, args) => { const value = await this.api.request(operation, args); this.resources.invalidate(); await this.refresh(true); return value; },
    };
  }
  private login(failure?: unknown): void {
    this.generation++; this.page?.dispose(); this.page = undefined;
    if (this.timer) clearInterval(this.timer);
    this.resources.cancel(); this.section = 'overview'; this.id = undefined;
    for (const dialog of document.querySelectorAll<HTMLDialogElement>('dialog')) dialog.close();
    const card = el('form', 'login-card'); const logo = el('img'); logo.src = '/logo.svg'; logo.alt = 'RustFRP';
    card.append(el('div', 'login-brand', logo, el('h1', '', 'RustFRP')));
    const server = el('input'); server.type = 'url'; server.name = 'server'; server.value = localStorage.getItem('rustfrp.server') ?? ''; server.placeholder = 'https://controller.example.com';
    const name = el('input'); name.name = 'name'; name.autocomplete = 'username'; name.required = true;
    const password = el('input'); password.name = 'password'; password.type = 'password'; password.autocomplete = 'current-password'; password.required = true;
    for (const [label, control] of [...(this.host.desktop ? [['Controller 地址', server] as const] : []), ['账号', name] as const, ['密码', password] as const]) {
      control.id = `login-${control.name}`; const caption = el('label', '', label); caption.htmlFor = control.id; card.append(el('div', 'form-field', caption, control));
    }
    const error = el('p', 'form-error'); error.setAttribute('role', 'alert'); error.hidden = !failure;
    if (failure instanceof Error) error.textContent = failure.message;
    const submit = button('登录', () => {}, { primary: true, id: 'login-submit' }); submit.type = 'submit'; card.append(error, submit);
    if (this.host.desktop) card.append(button('管理本机', () => this.navigate('local'), { id: 'login-local' }));
    card.addEventListener('submit', async event => {
      event.preventDefault(); if (this.busy) return; this.busy = true; submit.disabled = true; submit.setAttribute('aria-busy', 'true'); error.hidden = true;
      try {
        if (this.host.desktop) { await this.host.call('selectController', { url: server.value }); localStorage.setItem('rustfrp.server', server.value); }
        const value = record(await this.api.request('login', { body: { name: name.value.trim(), password: password.value } }));
        this.admin = { ...record(value.admin), role: role(record(value.admin).role) } as Admin; password.value = ''; this.shell();
      } catch (reason) { error.textContent = reason instanceof Error ? reason.message : String(reason); error.hidden = false; }
      finally { this.busy = false; submit.disabled = false; submit.setAttribute('aria-busy', 'false'); }
    });
    this.root.replaceChildren(el('div', 'login-screen', card)); name.focus();
  }
  private shell(): void {
    const sidebar = el('aside', 'sidebar'); const logo = el('img'); logo.src = '/logo.svg'; logo.alt = 'RustFRP';
    const collapse = button('«', () => {
      const closed = document.documentElement.dataset.collapsed !== 'true'; document.documentElement.dataset.collapsed = String(closed); localStorage.setItem('rustfrp.collapsed', String(closed));
      if (this.host.desktop) void this.host.call('savePreferences', { sidebar_collapsed: closed }).catch(error => this.showError(error));
    }); collapse.className = 'collapse'; collapse.setAttribute('aria-label', '展开或收起菜单');
    sidebar.append(el('div', 'brand', logo, el('span', 'brand-name', 'RustFRP'), collapse));
    const menu = el('nav', 'nav-main'); menu.setAttribute('aria-label', '主菜单'); const foot = el('div', 'nav-foot'); const bottom = el('nav', 'bottom-nav'); bottom.setAttribute('aria-label', '底部导航');
    const sections: Section[] = this.admin ? ['overview', 'nodes', 'clients', 'tunnels', ...(allowed(this.admin.role, 'admin') ? ['audit' as const] : []), 'settings'] : ['local', 'settings'];
    const nav = (section: Section) => {
      const item = el('button', 'nav-item', icon(section), el('span', 'nav-label', titles[section])); item.type = 'button'; item.dataset.section = section; item.title = titles[section];
      item.addEventListener('click', () => this.navigate(section)); return item;
    };
    for (const section of sections) { (section === 'settings' ? foot : menu).append(nav(section)); bottom.append(nav(section)); }
    if (this.host.desktop && this.admin) foot.append(nav('local'));
    sidebar.append(menu, el('div', 'connection-state', el('span', 'status-dot'), this.admin ? '已连接控制器' : '本机模式'), foot);
    const header = el('header', 'page-header'); const heading = el('h1'); heading.dataset.testid = 'page-title'; heading.setAttribute('data-tauri-drag-region', '');
    this.actions = el('div', 'actions'); header.append(heading, this.actions);
    this.content = el('main', 'workspace-body'); this.content.style.display = 'contents';
    const work = el('div', 'workspace', header, this.content); this.root.replaceChildren(el('div', 'app-shell', sidebar, work), bottom);
    this.mount(); if (this.timer) clearInterval(this.timer);
    this.timer = setInterval(() => { if (!document.hidden && !document.querySelector('dialog[open]')) void this.refresh(); }, 5000);
  }
  navigate(section: Section, id?: string): void {
    if ((section !== 'local' && section !== 'settings' && !this.admin) || (section === 'audit' && !allowed(this.admin?.role, 'admin'))) return;
    this.section = section; this.id = id; this.resources.cancel();
    if (this.host.desktop) {
      localStorage.setItem('rustfrp.page', JSON.stringify({ section, id }));
      if (section !== 'settings') void this.host.call('savePreferences', { local_mode: section === 'local' }).catch(error => this.showError(error));
    }
    if (!this.content?.isConnected) this.shell(); else this.mount();
  }
  private restorePage(): void {
    if (!this.host.desktop) return;
    try {
      const saved = record(JSON.parse(localStorage.getItem('rustfrp.page') ?? '{}'));
      if (typeof saved.section === 'string' && Object.hasOwn(titles, saved.section) && (this.admin || saved.section === 'local' || saved.section === 'settings') && (saved.section !== 'audit' || allowed(this.admin?.role, 'admin'))) {
        this.section = saved.section as Section; this.id = typeof saved.id === 'string' && saved.id.length <= 256 ? saved.id : undefined;
      }
    } catch { localStorage.removeItem('rustfrp.page'); }
  }
  private mount(): void {
    this.page?.dispose(); this.page = undefined; const context = this.context(), generation = ++this.generation;
    this.content?.replaceChildren(empty('正在加载', '正在加载页面…'));
    const heading = this.root.querySelector('[data-testid=page-title]'); if (heading) heading.textContent = titles[this.section];
    this.actions?.replaceChildren();
    for (const item of this.root.querySelectorAll<HTMLElement>('[data-section]')) { const selected = item.dataset.section === this.section; item.classList.toggle('selected', selected); item.setAttribute('aria-current', selected ? 'page' : 'false'); }
    void loadPage(context, this.section, this.id).then(page => {
      if (this.disposed || generation !== this.generation) { page.dispose(); return; }
      this.page = page; this.content?.replaceChildren(page.element);
      this.actions?.replaceChildren(...page.actions, button('刷新', () => this.refresh(true), { icon: 'refresh', id: 'refresh-page' }));
      void this.refresh();
    }).catch(error => { if (!this.disposed && generation === this.generation) { this.content?.replaceChildren(empty('页面加载失败', error instanceof Error ? error.message : '请重试', button('重试', () => this.mount()))); } });
  }
  private async refresh(force = false): Promise<void> {
    const current = this.page; if (!current || this.disposed) return;
    try { await current.refresh(force); }
    catch (error) { if (current === this.page) this.showError(error); }
  }
  private showError(error: unknown): void { toast(error instanceof Error ? error.message : String(error), 'danger'); }
  private async logout(): Promise<void> {
    try { await this.api.request('logout', { body: {} }); }
    catch (error) { this.showError(error); }
    finally { this.admin = null; this.resources.clear(); if (this.host.desktop) void this.host.call('savePreferences', { local_mode: false }).catch(error => this.showError(error)); this.login(); }
  }
  dispose(): void { this.disposed = true; this.stopWindow(); clearInterval(this.timer); document.removeEventListener('visibilitychange', this.visibility); this.page?.dispose(); this.resources.dispose(); this.api.close(); this.root.replaceChildren(); }
}
