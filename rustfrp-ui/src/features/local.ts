import type { Context, Page } from '../app/context';
import { record, text, bytes, records, address } from '../api/models';
import { el, intro, panel, details, empty, badge, button, toast, value } from '../components/dom';
import { guarded, View } from '../components/operations';
import { localSettings, localConfigEditor } from './local-config';
import { connectComputer } from './local-connect';
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
  let disposed = false; let generation = 0; let running = false; let configured = false; let standalone = false; let enrolled = false; let legacyLogs = false;
  let tab: 'status' | 'tunnels' | 'logs' = 'status';
  const manualStart = () => {
    const dialog = new FormDialog('使用管理员提供的接入令牌', [{ key: 'enrollToken', label: '一次性接入令牌', kind: 'password', secret: true, required: true, maxBytes: 2048, description: '仅供管理员已提供接入地址、证书与令牌时使用。普通接入请取消并点击“连接此电脑”。' }], async (values, current) => {
      await context.host.call('localStart', { enrollToken: values.enrollToken ?? '' }); current.finish(); toast('本机 Agent 已启动'); await context.refresh();
    }, '启动');
    dialog.element.dataset.sensitive = 'true';
  };
  const start = button('连接此电脑', guarded(context, async () => {
    if (!standalone && !enrolled) { await connectComputer(context); return; }
    await context.host.call('localStart', { enrollToken: '' }); toast('本机 Agent 已启动'); await context.refresh();
  }), { primary: true, id: 'local-start', disabled: () => running });
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
    start.textContent = standalone ? '启动 Agent' : enrolled ? '启动连接' : context.admin ? '连接此电脑' : '登录并连接此电脑'; start.disabled = running; stop.disabled = !running; settings.disabled = running; edit.disabled = !standalone; diagnostics.disabled = !running;
    stop.hidden = !running; edit.hidden = !standalone; diagnostics.hidden = !running; tabs.hidden = !running;
    for (const [name, control] of tabButtons) { control.classList.toggle('selected', name === tab); control.setAttribute('aria-pressed', String(name === tab)); }
  }
  async function refresh(): Promise<void> {
    if (!context.host.desktop) { tools.hidden = true; tabs.hidden = true; data.replaceChildren(); view.update(false, () => [empty('本机管理需要桌面客户端', 'Web 面板继续管理远程 Controller 中的客户端与隧道。')]); return; }
    const request = ++generation; const selected = tab;
    const [status, prefs, capabilities] = await Promise.all([context.host.call('localStatus'), context.host.call('preferences'), context.host.call('info')]); if (disposed || request !== generation) return;
    const configuration = record(prefs).local ? record(record(prefs).local) : {}; standalone = !!configuration.configFile;
    configured = !!(configuration.binary && configuration.stateDir && (standalone || configuration.controller)); enrolled = record(capabilities ?? {}).local_enrolled === true; running = status !== null; updateControls();
    if (status === null) {
      data.replaceChildren(); table.update([]); logs.clear(); legacyLogs = false;
      view.update({ prefs, enrolled }, () => {
        if (!standalone && !enrolled) {
          const manual = configured ? el('details', 'connection-advanced', el('summary', '', '管理员手动接入'), details([['已保存的接入地址', configuration.controller]]), el('p', 'muted-text', '保存地址还不代表已接入。登录后会从所选节点自动更新地址和证书。'), button('使用管理员提供的接入令牌', manualStart, { id: 'local-manual-enroll' })) : null;
          return [panel('此电脑尚未接入', 'local', el('p', 'muted-text', context.admin ? '选择服务端节点后即可连接，应用会自动处理接入凭据。' : '登录管理服务，选择节点即可连接。无需查找或填写一次性令牌。'), !context.admin ? button('登录管理服务', () => (context.loginLocal ?? context.logout)(), { id: 'local-login-entry' }) : null, manual)];
        }
        const advanced = el('details', 'connection-advanced', el('summary', '', '查看本机文件与证书设置'), details([['状态目录', configuration.stateDir], ['Agent 程序', configuration.binary], ['可信 CA 文件', configuration.caFile], ['证书指纹', configuration.tlsPin]]));
        return [panel(standalone ? '本机 Agent 未启动' : '连接已停止', 'local', el('p', 'muted-text', standalone ? '配置文件已选择，点击“启动 Agent”运行。' : '此电脑已有接入身份，点击“启动连接”即可恢复，无需再次输入令牌。'), details([['连接方式', standalone ? 'frpc 配置文件' : '接入管理服务'], [standalone ? '配置文件' : '接入地址', standalone ? configuration.configFile : configuration.controller]]), advanced)];
      }); return;
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
