import type { Context, Page } from '../app/context';
import { allowed, date, disabled, record, records, roleNames, roles, text, tokenStatus, type Admin, type EnrollToken, type RecordValue } from '../api/models';
import { badge, button, details, el, intro, panel, value } from '../components/dom';
import { confirmation, FormDialog, reason } from '../components/forms';
import { edit, guarded, secretDialog, View } from '../components/operations';
import { LogView } from '../components/logs';
import { ResourceTable } from '../components/table';
import { enroll } from './enrollment';
import { AppUpdate } from './app-update';

const sections: [string, string][] = [['account', '账号'], ['appearance', '外观'], ['enroll', '接入令牌'], ['api', 'API Token'], ['admins', '管理员'], ['logs', '控制器日志'], ['desktop', '桌面设置'], ['about', '关于']];
export function settingSections(context: Context): [string, string][] {
  return sections.filter(([id]) => id === 'enroll' || id === 'logs' ? allowed(context.admin?.role, 'admin') : id === 'api' || id === 'admins' ? allowed(context.admin?.role, 'owner') : id === 'desktop' ? context.host.desktop : true);
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
  const table = new ResourceTable(page === 'admins' ? ['账号', '角色', '状态', '创建时间', '操作'] : page === 'api' ? ['名称', '权限范围', '到期时间', '状态', '操作'] : ['名称 / 类型', '创建信息', '到期时间', '状态', '操作'], 900);
  const logs = new LogView(); const actions: HTMLElement[] = [];
  const createApi = () => { new FormDialog('创建 API Token', [
    { key: 'name', label: '名称', required: true, maxBytes: 100 }, { key: 'scope', label: '权限范围', kind: 'select', value: 'read', options: [['read', '只读'], ['tunnels', '隧道管理'], ['admin', '设备与隧道管理']] },
    { key: 'days', label: '有效天数', kind: 'number', min: 1, max: 365, value: 30 }, reason,
  ], async (body, dialog) => { const result = record(await context.write('createApiToken', { body })); dialog.finish(); secretDialog(context, '保存 API Token', { 'Bearer Token': text(result.token, '') }, '原文仅在本次显示。关闭后无法再次查看，请安全保存。'); }); };
  const createAdmin = () => { edit('新增管理员', [
    { key: 'name', label: '账号', required: true, maxLength: 64 }, { key: 'role', label: '角色', kind: 'select', options: roles.map(role => [role, roleNames[role]]), value: 'viewer' },
    { key: 'password', label: '初始密码', kind: 'password', required: true, description: '至少 10 个字符，密码仅提交到 Controller。', maxBytes: 1024 },
  ], body => context.write('createAdmin', { body })); };
  if (page === 'api') actions.push(button('创建 API Token', createApi, { primary: true, id: 'new-api-token' }));
  if (page === 'admins') actions.push(button('新增管理员', createAdmin, { primary: true, id: 'new-admin' }));
  if (page === 'enroll') actions.push(button('创建接入令牌', guarded(context, () => enroll(context)), { primary: true, id: 'new-enroll-token' }));
  if (page === 'account') {
    content.append(intro('账号与连接', '权限由 Controller 校验，设备身份与管理会话相互独立。', 'clients'), panel('当前账号', 'clients', details([['账号', context.admin?.name ?? '未登录'], ['角色', context.admin ? roleNames[context.admin.role] : '本机模式']]), context.admin ? button('退出登录', () => context.logout()) : null,
      context.host.desktop ? el('div', 'actions', button('管理本机', () => context.navigate('local')), button('切换服务器', () => context.logout(), { id: 'switch-controller' })) : null), panel('外观', 'settings', themes(context)));
  } else if (page === 'appearance') content.append(intro('外观', '使用原生字体和共享配色，支持系统深浅色。', 'settings'), panel('主题', 'settings', themes(context)));
  else if (page === 'logs') content.append(intro('控制器日志', '实时读取有界日志环；覆盖缺口明确标示。', 'audit'), logs.notice, logs.element);
  else if (page === 'api' || page === 'admins' || page === 'enroll') {
    content.append(intro(page === 'api' ? '自动化 API Token' : page === 'admins' ? '管理员与角色' : '接入令牌', page === 'api' ? '当前账号最近 100 个令牌。原文只在创建成功时显示一次。' : page === 'admins' ? '保护当前账号的角色和状态，以及最后一位启用的所有者。' : '单次设备接入，列表只显示状态；凭据不能从列表恢复。', page === 'admins' ? 'clients' : 'key'), table.element);
  }
  function adminEdit(admin: Admin, all: Admin[]): void {
    const own = admin.id === context.admin?.id; const lastOwner = admin.role === 'owner' && !disabled(admin) && all.filter(item => item.role === 'owner' && !disabled(item)).length === 1;
    edit('修改管理员', [
      { key: 'role', label: '角色', kind: 'select', options: roles.map(role => [role, roleNames[role]]), value: admin.role, readonly: own || lastOwner, description: own ? '不能修改当前账号的角色。' : lastOwner ? '不能降级最后一位启用的所有者。' : undefined },
      { key: 'disabled', label: '停用账号', kind: 'checkbox', value: disabled(admin), readonly: own || lastOwner, description: own ? '不能停用当前账号。' : undefined },
      { key: 'password', label: '重置密码', kind: 'password', secret: true, description: '留空保持原密码；新密码至少 10 个字符。', maxBytes: 1024 },
    ], body => { if (Object.keys(body).length === 1) throw new Error('请填写要修改的内容。'); return context.write('patchAdmin', { id: admin.id, body }); });
  }
  async function refresh(force = false): Promise<void> {
    if (page === 'logs') { const response = await context.api.request('logs', { query: { after: String(logs.cursor) } }, abort.signal); if (!disposed) logs.append(response); return; }
    const age = force ? 0 : 4000;
    if (page === 'admins') {
      const admins = records<Admin>(await context.resources.read('admins', 'admins', {}, age)); if (disposed) return;
      table.update(admins.map(admin => ({ id: admin.id, signature: JSON.stringify([admin, admins.filter(item => item.role === 'owner' && !disabled(item)).length]), cells: () => [value(admin.name, admin.id === context.admin?.id ? '当前账号' : undefined), badge(roleNames[admin.role], 'blue'), badge(disabled(admin) ? '已停用' : '启用', disabled(admin) ? 'muted' : 'success'), value(date(admin.created_at)), button('修改 / 重置密码', () => adminEdit(admin, admins), { id: `edit-admin-${admin.id}` })] })));
    } else if (page === 'api') {
      const tokens = records<RecordValue>(await context.resources.read('api-tokens', 'apiTokens', {}, age)); if (disposed) return;
      table.update(tokens.map(token => ({ id: String(token.id), signature: JSON.stringify([token, Number(token.expires_at) <= Date.now()]), cells: () => {
        const expired = Number(token.expires_at) <= Date.now(); const revoked = token.revoked_at != null;
        return [value(token.name), value({ read: '只读', tunnels: '隧道管理', admin: '设备与隧道管理' }[String(token.scope)] ?? token.scope, undefined, 'blue'), value(date(token.expires_at)), badge(revoked ? '已撤销' : expired ? '已过期' : '有效', revoked || expired ? 'muted' : 'success'), revoked ? value('—') : button('撤销', () => { confirmation('撤销 API Token', '使用该令牌的自动化调用将立即失效。', reason => context.write('revokeApiToken', { id: String(token.id), body: { reason } }).then(() => {})); }, { danger: true, id: `revoke-api-token-${token.id}` })];
      }})));
    } else if (page === 'enroll') {
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
      ]);
    }
  }
  return { element, actions, refresh, dispose() { disposed = true; abort.abort(); table.dispose(); logs.clear(); updater?.dispose(); } };
}
