import type { Context, Page } from '../app/context';
import { record, records, bytes, numeric, text, type Client, type Tunnel, type NodeInfo } from '../api/models';
import { el, icon, panel, details, button, empty, value, select } from '../components/dom';
import { View } from '../components/operations';
import { trafficSlots, trafficChart } from './traffic';

export function overviewPage(context: Context): Page {
  const element = el('div', 'overview-content');
  const metrics = el('div', 'metrics'); const nodePanel = el('div', 'detail-grid'); const recent = el('div');
  const history = panel('流量历史', 'transfer', el('p', 'muted-text', '正在读取流量历史…'));
  element.append(metrics, history, nodePanel, panel('最近的客户端', 'clients', recent));
  let disposed = false;
  let hours = '24';
  let clientId = ''; let tunnelId = ''; let generation = 0;
  const metricsView = new View(metrics); const nodesView = new View(nodePanel); const recentView = new View(recent);
  const graph = el('div'); const graphView = new View(graph);
  const clientFilter = select([['', '全部客户端']], '', value => { clientId = value; tunnelId = ''; void reload(); }, 'traffic-client');
  const tunnelFilter = select([['', '全部隧道']], '', value => { tunnelId = value; void reload(); }, 'traffic-tunnel');
  const ranges = ['24', '168'].map(range => { const item = button(range === '24' ? '24 小时' : '7 天', async () => { hours = range; generation++; await context.refresh(); }, { compact: true }); item.dataset.range = range; return item; });
  const toolbar = el('div', 'filters', ...ranges, clientFilter, tunnelFilter);
  history.replaceChildren(el('h3', 'panel-heading', icon('transfer'), '流量历史'), toolbar, graph,
    el('div', 'legend', el('span', '', '入站：公网 → 本机'), el('span', '', '出站：本机 → 公网')),
    el('p', 'chart-note', '按小时汇总已观测流量；空白表示未采样，虚线表示观测不完整。约每 5 秒采样，时间按本机时区显示。'));
  async function reload(): Promise<void> { generation++; await context.refresh(); }
  function options(control: HTMLSelectElement, values: [string, string][], selected: string): void {
    const signature = JSON.stringify(values); if (control.dataset.options !== signature) {
      control.replaceChildren(...values.map(([key, label]) => { const option = el('option', '', label); option.value = key; return option; })); control.dataset.options = signature;
    }
    control.value = selected;
  }
  async function refresh(force = false): Promise<void> {
    const age = force ? 0 : 4000;
    const current = generation;
    const [summary, nodeValues, clientValues, tunnelValues] = await Promise.all([
      context.resources.read('overview', 'overview', {}, age), context.resources.read('nodes', 'nodes', {}, age),
      context.resources.read('clients', 'clients', {}, age), context.resources.read('tunnels', 'tunnels', {}, age),
    ]);
    if (disposed || current !== generation) return;
    const info = record(summary); const counts = record(info.counts); const traffic = record(info.traffic); const day = record(info.traffic_24h);
    const clients = records<Client>(clientValues); const nodes = records<NodeInfo>(nodeValues); const tunnels = records<Tunnel>(tunnelValues);
    const process = info.process && typeof info.process === 'object' && !Array.isArray(info.process) ? info.process : {};
    const cards: [string, string, string, string][] = [
      ['在线客户端', String(numeric(info.clients_online) ?? '—'), `共 ${clients.length} 台`, 'clients'],
      ['运行中的隧道', String(numeric(counts.tunnels_running) ?? tunnels.filter(item => item.state === 'running').length), `共 ${tunnels.length} 条`, 'tunnels'],
      ['待使用的接入令牌', String(numeric(counts.tokens_pending) ?? '—'), '', 'key'],
      ['当前连接', String(numeric(traffic.connections) ?? '—'), '', 'activity'],
      ['24 小时流量', numeric(day.bytes_in) === null || numeric(day.bytes_out) === null ? '—' : bytes(Number(day.bytes_in) + Number(day.bytes_out)), `↓ ${bytes(day.bytes_in)} · ↑ ${bytes(day.bytes_out)}`, 'transfer'],
      ['控制器版本', text(info.version), `内存 ${bytes(process.rss_bytes)}`, 'nodes'],
    ];
    metricsView.update(cards, () => cards.map(([label, amount, hint, glyph]) => {
      const display = el('div', 'metric-value', amount); display.title = amount;
      const caption = el('div', 'metric-hint', hint); caption.title = hint;
      return el('div', 'metric-card', el('div', 'metric-label', icon(glyph), label), display, caption);
    }));
    nodesView.update(nodes, () => nodes.map(node => panel(node.builtin ? '内置服务端节点' : node.name, 'nodes', details([
      ['状态', node.disabled_at != null ? '已停用' : node.online ? '在线' : '离线'], ['公网地址', `${node.public_host}:${node.edge_port}`],
      ['已接入客户端', String(node.clients)], ['版本', node.version],
    ]))));
    recentView.update(clients.slice(0, 5), () => clients.length ? clients.slice(0, 5).map(client => el('div', 'summary', value(client.name, client.online ? '在线' : '离线'), button('查看详情', () => context.navigate('clients', client.id)))) : [empty('还没有客户端', '接入客户端后查看连接状态。')]);
    if (clientId && !clients.some(client => client.id === clientId)) clientId = '';
    const available = tunnels.filter(tunnel => !clientId || tunnel.client_id === clientId);
    if (tunnelId && !available.some(tunnel => tunnel.id === tunnelId)) tunnelId = '';
    options(clientFilter, [['', '全部客户端'], ...clients.map(client => [client.id, client.name] as [string, string])], clientId);
    options(tunnelFilter, [['', '全部隧道'], ...available.map(tunnel => [tunnel.id, tunnel.name] as [string, string])], tunnelId);
    for (const item of ranges) item.classList.toggle('selected', item.dataset.range === hours);
    const query = { hours, ...(clientId ? { client_id: clientId } : {}), ...(tunnelId ? { tunnel_id: tunnelId } : {}) };
    const trafficValue = await context.resources.read(`traffic-${JSON.stringify(query)}`, 'traffic', { query }, age);
    if (disposed || current !== generation) return;
    const data = trafficSlots(trafficValue);
    graphView.update(trafficValue, () => [data.slots.some(Boolean) ? trafficChart(data) : empty('暂无流量采样', '未采样不代表零流量。')]);
  }
  return { element, actions: [], refresh, dispose() { disposed = true; } };
}
