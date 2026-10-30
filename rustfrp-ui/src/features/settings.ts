import type { Context, Page } from '../app/context';
import { allowed, date, record, records, tokenStatus, type EnrollToken } from '../api/models';
import { badge, button, details, el, intro, panel, value } from '../components/dom';
import { confirmation } from '../components/forms';
import { guarded, View } from '../components/operations';
import { LogView } from '../components/logs';
import { ResourceTable } from '../components/table';
import { enroll } from './enrollment';
import { AppUpdate } from './app-update';

const sections: [string, string][] = [['account', '管理连接'], ['appearance', '外观'], ['enroll', '设备接入令牌'], ['logs', '控制器日志'], ['desktop', '桌面设置'], ['about', '关于']];
export function settingSections(context: Context): [string, string][] {
  return sections.filter(([id]) => id === 'enroll' || id === 'logs' ? allowed(context.admin?.role, 'admin') : id === 'desktop' ? context.host.desktop : true);
}
export function theme(context: Context, choice: string): void {
  localStorage.setItem('rustfrp.theme', choice);
  document.documentElement.dataset.theme = choice === 'system' ? matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light' : choice;
  if (context.host.desktop) void context.host.call('savePreferences', { theme: choice }).catch(context.showError);
}
function themes(context: Context): HTMLElement {
  return el('div', 'actions', ...['light', 'dark', 'system'].map(choice => button({ light: '浅色', dark: '深色', system: '跟随系统' }[choice] ?? choice, () => theme(context, choice))));
}
export function settingsPage(context: Context, id = 'account'): Page {
  let updater: AppUpdate | undefined;
  const accessible = settingSections(context); const page = accessible.some(([key]) => key === id) ? id : 'account';
  const element = el('div', 'page-content'); const content = el('div'); const view = new View(content);
  element.append(el('nav', 'setting-tabs', ...accessible.map(([key, label]) => { const item = button(label, () => context.navigate('settings', key), { id: `setting-${key}` }); item.classList.toggle('selected', key === page); item.setAttribute('aria-current', key === page ? 'page' : 'false'); return item; })), content);
  let disposed = false; const abort = new AbortController();
  const table = new ResourceTable(['名称 / 类型', '创建信息', '到期时间', '状态', '操作'], 900);
  const logs = new LogView(); const actions: HTMLElement[] = [];
  if (page === 'enroll') actions.push(button('创建接入令牌', guarded(context, () => enroll(context)), { primary: true, id: 'new-enroll-token' }));
  if (page === 'account') {
    content.append(intro('管理连接', '管理 token 在服务端 [admin] 中配置，修改后重启服务生效。', 'clients'), panel('当前连接', 'clients', details([['管理服务器', context.host.desktop ? localStorage.getItem('rustfrp.server') || 'http://127.0.0.1:9674' : location.origin], ['认证方式', '单管理 token'], ['状态', context.admin ? '已连接管理服务器' : '未连接']]), button('断开连接', () => context.logout()), button('切换服务器', () => context.logout(), { id: 'switch-controller' }), el('p', 'muted-text', '管理 token 与设备接入令牌独立。应用不保存管理 token 原文。')), panel('外观', 'settings', themes(context)));
  } else if (page === 'appearance') content.append(intro('外观', '使用原生字体和共享配色，支持系统深浅色。', 'settings'), panel('主题', 'settings', themes(context)));
  else if (page === 'logs') content.append(intro('控制器日志', '实时读取有界日志环；覆盖缺口明确标示。', 'audit'), logs.notice, logs.element);
  else if (page === 'enroll') content.append(intro('设备接入令牌', '仅用于设备首次接入，与管理 token 不通用。', 'key'), table.element);
  async function refresh(force = false): Promise<void> {
    if (page === 'logs') { const response = await context.api.request('logs', { query: { after: String(logs.cursor) } }, abort.signal); if (!disposed) logs.append(response); return; }
    const age = force ? 0 : 4000;
    if (page === 'enroll') {
      const tokens = records<EnrollToken>(await context.resources.read('enroll-tokens', 'enrollTokens', {}, age)); if (disposed) return;
      table.update(tokens.map(token => ({ id: token.id, signature: JSON.stringify([token, token.expires_at <= Date.now()]), cells: () => {
        const [state, tone] = tokenStatus(token);
        return [value(token.name, token.kind === 'node' ? '服务端节点' : '客户端'), value(token.created_by, date(token.created_at)), value(date(token.expires_at)), badge(state, tone), token.revoked_at != null || token.used_at != null ? value('—') : button('撤销', () => { confirmation('撤销接入令牌', '撤销后不能再用此令牌接入设备；已经接入的设备不受影响。', reason => context.write('revokeEnrollToken', { id: token.id, query: { reason } }).then(() => {})); }, { danger: true, id: `revoke-enroll-${token.id}` })];
      }})));
    } else if (page === 'about') {
      const info = record(await context.host.call('info')); if (disposed) return;
      view.update(info, () => { const panels = [intro('RustFRP', '轻量的 Rust 隧道与集中管理。', 'nodes'), panel('版本', 'nodes', details([['界面版本', info.version], ['运行环境', context.host.desktop ? '系统 WebView 桌面客户端' : '浏览器 Web 面板']]), button('官网', guarded(context, () => context.host.call('openUrl', { url: 'https://rustfrp.com' }))), button('Release', guarded(context, () => context.host.call('openUrl', { url: 'https://github.com/yuhuotech/RustFRP/releases' }))))]; if (context.host.desktop) { updater?.dispose(); updater = new AppUpdate(context); panels.push(updater.element); } return panels; });
      await updater?.refresh();
    } else if (page === 'desktop') {
      const [preferences, startup] = await Promise.all([context.host.call('preferences'), context.host.call('startup')]); const prefs = record(preferences); if (disposed) return;
      view.update([prefs, startup], () => [
        intro('桌面设置', '后台 Agent 独立于窗口运行。', 'local'),
        panel('系统集成', 'settings', ...[
        ['close_to_tray', '关闭窗口保留托盘'], ['notifications', '设备状态通知'], ['lightweight', '隐藏后释放窗口'],
      ].map(([key, label]) => {
        const check = el('input'); check.type = 'checkbox'; check.checked = prefs[key] !== false; check.setAttribute('aria-label', label);
        check.addEventListener('change', () => {
          const previous = prefs[key] !== false; check.disabled = true; const enabled = check.checked;
          void context.host.call(key === 'notifications' ? 'setNotifications' : 'savePreferences', key === 'notifications' ? { enabled } : { [key]: enabled })
            .then(() => context.refresh()).catch(error => { check.checked = previous; context.showError(error); }).finally(() => { check.disabled = false; });
        }); return el('label', 'preference-row', check, label);
      }), button(startup === true ? '关闭开机启动' : '开启开机启动', guarded(context, async () => { await context.host.call('setStartup', { enabled: startup !== true }); await context.refresh(); }), { id: 'desktop-startup' })),
        panel('窗口释放', 'activity', el('p', 'muted-text', '隐藏窗口 45 秒后释放 WebView；未保存表单、一次性凭据和正在执行的操作会阻止释放。重新打开恢复所在页面。通知是否可显示由系统通知设置控制。')),
        el('details', 'connection-advanced', el('summary', '', '本机维护（高级）'), button('打开本机维护', () => context.navigate('local'), { id: 'local-maintenance' })),
      ]);
    }
  }
  return { element, actions, refresh, dispose() { disposed = true; abort.abort(); table.dispose(); logs.clear(); updater?.dispose(); } };
}
