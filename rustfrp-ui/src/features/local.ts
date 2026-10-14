import type { Context, Page } from '../app/context';
import { record, text, bytes, records, address } from '../api/models';
import { el, intro, panel, details, empty, badge, button, toast, value } from '../components/dom';
import { guarded, View } from '../components/operations';
import { localSettings, localConfigEditor } from './local-config';
import { FormDialog } from '../components/forms';
import { LogView } from '../components/logs';
import { ResourceTable } from '../components/table';

export function localPage(context: Context): Page {
  const element = el('div', 'page-content'); const content = el('div'); const tools = el('div', 'filters page-toolbar');
  const view = new View(content);
  const table = new ResourceTable(['名称 / 类型', '本地目标', '远程端口（配置）'], 620); const logs = new LogView(256);
  const data = el('div'); const tabs = el('div', 'setting-tabs'); tabs.setAttribute('aria-label', '本机信息分类');
  const tunnelRevision = el('p', 'muted-text'); const tunnelPanel = panel('已应用隧道', 'tunnels', tunnelRevision, el('p', 'muted-text', '本机摘要展示已应用的代理配置；公网实际入口和远程管理操作在 Controller 面板中查看。'), table.element);
  element.append(intro('本机 Agent', '界面关闭后 Agent 继续在后台运行。', 'local'), tools, content, tabs, data);
  let disposed = false; let generation = 0; let running = false; let configured = false; let standalone = false; let legacyLogs = false;
  let tab: 'status' | 'tunnels' | 'logs' = 'status';
  const start = button('启动 Agent', guarded(context, () => {
    const dialog = new FormDialog('启动本机 Agent', standalone ? [] : [{ key: 'enrollToken', label: '一次性接入令牌', kind: 'password', secret: true, maxBytes: 2048, description: '首次接入时填写。已有身份可留空；令牌只用于本次接入。' }], async (values, current) => {
      await context.host.call('localStart', { enrollToken: values.enrollToken ?? '' }); current.finish(); toast('本机 Agent 已启动'); await context.refresh();
    }, '启动');
    if (!standalone) dialog.element.dataset.sensitive = 'true';
    dialog.content.prepend(el('p', 'muted-text', standalone ? '使用完整 frpc 配置启动独立客户端。' : '接入成功后会交给不含接入令牌的后台服务运行。'));
  }), { primary: true, id: 'local-start', disabled: () => running || !configured });
  const stop = button('停止 Agent', guarded(context, () => {
    const dialog = new FormDialog('停止本机 Agent', [], async (_values, current) => { await context.host.call('localStop'); current.finish(); logs.clear(); toast('本机 Agent 已停止'); await context.refresh(); }, '停止');
    dialog.content.append(el('p', 'muted-text', '停止后本机隧道将中断，可再次启动恢复。'));
  }), { danger: true, id: 'local-stop', disabled: () => !running });
  const settings = button('连接设置', guarded(context, () => localSettings(context)), { id: 'local-settings', disabled: () => running });
  const edit = button('编辑独立配置', guarded(context, () => localConfigEditor(context)), { id: 'local-config-edit', disabled: () => !standalone });
  const diagnostics = button('导出诊断', guarded(context, async () => { const result = record(await context.host.call('exportDiagnostics')); if (!text(result.path, '')) throw new Error('未取得诊断文件路径，请刷新确认 Agent 状态。'); toast(`脱敏诊断已导出：${result.path}`); }), { id: 'local-diagnostics', disabled: () => !running });
  tools.append(start, stop, settings, edit, diagnostics);
  const tabButtons = (['status', 'tunnels', 'logs'] as const).map(name => {
    const control = button({ status: '运行状态', tunnels: '已应用隧道', logs: 'Agent 日志' }[name], guarded(context, async () => { tab = name; await refresh(); }), { id: `local-tab-${name}` }); tabs.append(control); return [name, control] as const;
  });
  function updateControls(): void {
    start.disabled = running || !configured; stop.disabled = !running; settings.disabled = running; edit.disabled = !standalone; diagnostics.disabled = !running;
    for (const [name, control] of tabButtons) { control.classList.toggle('selected', name === tab); control.setAttribute('aria-pressed', String(name === tab)); }
  }
  async function refresh(): Promise<void> {
    if (!context.host.desktop) { tools.hidden = true; tabs.hidden = true; data.replaceChildren(); view.update(false, () => [empty('本机管理需要桌面客户端', 'Web 面板继续管理远程 Controller 中的客户端与隧道。')]); return; }
    const request = ++generation; const selected = tab;
    const [status, prefs] = await Promise.all([context.host.call('localStatus'), context.host.call('preferences')]); if (disposed || request !== generation) return;
    const configuration = record(prefs).local ? record(record(prefs).local) : {}; standalone = !!configuration.configFile;
    configured = !!(configuration.binary && configuration.stateDir && (standalone || configuration.controller)); running = status !== null; updateControls();
    if (status === null) {
      data.replaceChildren(); table.update([]); logs.clear(); legacyLogs = false;
      view.update(prefs, () => [empty('未发现本机 Agent', '配置受管接入或选择独立模式配置文件后启动。'), panel('本机连接配置', 'settings', details([['运行模式', standalone ? '独立客户端' : '受管客户端'], [standalone ? '配置文件' : 'Edge 接入地址', standalone ? configuration.configFile : configuration.controller], ['状态目录', configuration.stateDir], ['Agent 程序', configuration.binary]]))]); return;
    }
    const info = record(status); const traffic = info.traffic && typeof info.traffic === 'object' && !Array.isArray(info.traffic) ? info.traffic : {};
    view.update(status, () => [panel('连接状态', 'activity', badge(info.connected ? '已连接' : '未连接', info.connected ? 'success' : 'warning'), details([
      ['Agent 版本', text(info.agent_version)], ['进程 PID', String(info.pid)], ['已应用配置版本', String(info.revision)],
      ['隧道数量', String(info.proxies)], ['入站流量', bytes(traffic.bytes_in)], ['出站流量', bytes(traffic.bytes_out)], ['当前连接数', traffic.connections ?? '未上报'],
    ]))]);
    if (selected === 'status') { data.replaceChildren(); return; }
    if (selected === 'tunnels') {
      const response = record(await context.host.call('localTunnels')); if (disposed || request !== generation) return;
      const items = records(response.items); table.update(items.map(item => ({ id: text(item.id), signature: JSON.stringify(item), cells: () => [value(item.id, item.kind), value(address(item.local_ip, item.local_port)), value(item.remote_port === 0 ? '由服务端分配 / 未上报' : item.remote_port)] })));
      tunnelRevision.textContent = `配置版本 ${response.revision} · ${items.length} 条隧道`;
      if (!data.contains(table.element)) data.replaceChildren(tunnelPanel);
    } else {
      const response = record(await context.host.call('localLogs', legacyLogs || logs.cursor === 0 ? {} : { after: logs.cursor })); if (disposed || request !== generation) return;
      legacyLogs = response.cursor === undefined; if (legacyLogs) logs.snapshot(response); else logs.append(response);
      if (!data.contains(logs.element)) data.replaceChildren(panel('Agent 日志', 'audit', logs.notice, logs.element));
    }
  }
  return { element, actions: [], refresh, dispose() { disposed = true; generation++; table.dispose(); logs.clear(); } };
}
