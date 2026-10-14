import { record, text, type RecordValue, type Tunnel } from '../api/models';
import type { Field } from '../components/forms';

export const tunnelKinds = ['tcp', 'udp', 'http', 'https', 'stcp', 'sudp', 'tcpmux'];
export const templates: Record<string, [string, number, string]> = { ssh: ['SSH', 22, 'tcp'], rdp: ['RDP', 3389, 'tcp'], web: ['Web', 80, 'http'], mysql: ['MySQL', 3306, 'tcp'], postgres: ['PostgreSQL', 5432, 'tcp'] };
export const pluginKinds = ['socks5', 'http_proxy', 'static_file', 'unix_domain_socket', 'http2https', 'https2http', 'https2https', 'tls2raw'];
export function headers(source: string): RecordValue {
  const result: RecordValue = {};
  for (const line of source.split('\n')) {
    if (!line.trim()) continue;
    const colon = line.indexOf(':');
    if (colon <= 0) throw new Error('请求/响应头须按每行 Name: value 填写');
    const name = line.slice(0, colon).trim(); const value = line.slice(colon + 1).trim();
    if (!/^[!#$%&'*+.^_`|~\da-z-]+$/i.test(name) || /[\r\n\0]/.test(value)) throw new Error('请求/响应头含无效字符');
    result[name] = value;
  }
  return result;
}
function headerText(value: unknown): string {
  return value && typeof value === 'object' && !Array.isArray(value) ? Object.entries(value).map(([key, val]) => `${key}: ${val}`).join('\n') : '';
}
export function tunnelFields(clients: [string, string][], existing?: Tunnel, clientId?: string): Field[] {
  const source: RecordValue = existing ?? {};
  const health: RecordValue = source.health_check ? record(source.health_check) : {};
  const common = '基本信息'; const http = 'HTTP 路由与认证'; const transport = '连接与传输'; const lb = '负载均衡与健康检查'; const plugin = '本地插件';
  return [
    { key: 'client_id', label: '所属客户端', kind: 'select', options: clients, value: existing?.client_id ?? clientId ?? clients[0]?.[0] ?? '', readonly: !!existing || clientId !== undefined, required: true, group: common },
    { key: 'kind', label: '类型', kind: 'select', options: tunnelKinds.map(kind => [kind, kind.toUpperCase()]), value: existing?.kind ?? 'tcp', readonly: !!existing, group: common },
    { key: 'name', label: '名称', value: source.name ?? '', required: true, maxLength: 64, group: common },
    { key: 'local_ip', label: '本地目标地址', value: source.local_ip ?? '127.0.0.1', required: true, maxBytes: 253, group: common },
    { key: 'local_port', label: '本地端口', kind: 'number', value: source.local_port ?? 80, min: 1, max: 65535, group: common },
    { key: 'remote_port', label: '远程端口', kind: 'number', value: source.remote_port ?? 0, min: 0, max: 65535, description: '0 表示由节点自动分配。', group: common },
    { key: 'custom_domains', label: '自定义域名', kind: 'list', value: source.custom_domains ?? [], description: '每行一个域名；与子域名至少填写一项。', group: '域名' },
    { key: 'subdomain', label: '子域名', value: source.subdomain ?? '', maxBytes: 128, group: '域名' },
    { key: 'host_header_rewrite', label: '改写 Host', value: source.host_header_rewrite ?? '', group: http },
    { key: 'locations', label: '路径前缀', kind: 'list', value: source.locations ?? [], description: '每行一个以 / 开头的路径；留空匹配全部路径。', group: http },
    { key: 'http_user', label: 'Basic Auth 用户名', value: source.http_user ?? '', group: http },
    { key: 'http_password', label: 'Basic Auth 密码', kind: 'password', secret: !!existing, description: existing?.http_password_set ? '已设置；留空保持原密码。' : '未设置密码。', group: http },
    { key: 'clear_http_password', label: '清除已保存的 Basic Auth 密码', kind: 'checkbox', value: false, group: http },
    { key: 'route_by_http_user', label: '按 Basic 用户名选路', value: source.route_by_http_user ?? '', group: http },
    { key: 'request_headers', label: '请求头', kind: 'textarea', value: headerText(source.request_headers), description: '每行 Name: value。', group: http },
    { key: 'response_headers', label: '响应头', kind: 'textarea', value: headerText(source.response_headers), description: '每行 Name: value。', group: http },
    { key: 'multiplexer', label: 'TCPMUX 复用器', kind: 'select', options: [['httpconnect', 'HTTP CONNECT']], value: 'httpconnect', group: transport },
    { key: 'proxy_protocol', label: 'PROXY protocol', kind: 'select', options: [['0', '关闭'], ['1', 'v1'], ['2', 'v2']], value: String(source.proxy_protocol ?? 0), group: transport },
    { key: 'bandwidth_bps', label: '带宽上限（字节/秒）', kind: 'number', min: 0, max: 1099511627776, value: source.bandwidth_bps ?? 0, description: '0 表示不限。', group: transport },
    { key: 'bandwidth_server', label: '由服务端执行限速', kind: 'checkbox', value: source.bandwidth_server ?? false, group: transport },
    { key: 'max_connections', label: '并发连接 / 活跃 UDP 源上限', kind: 'number', min: 0, max: 1000000, value: source.max_connections ?? 0, description: '0 表示不限。', group: transport },
    { key: 'use_encryption', label: '附加 AES-256-GCM 加密', kind: 'checkbox', value: source.use_encryption ?? false, group: transport },
    { key: 'use_compression', label: '有界 LZ4 压缩', kind: 'checkbox', value: source.use_compression ?? false, group: transport },
    { key: 'lb_group', label: '负载均衡组', value: source.lb_group ?? '', group: lb },
    { key: 'lb_group_key', label: '组密钥', kind: 'password', secret: !!existing, description: existing?.lb_group_key_set ? '已设置；留空保持原密钥，退出组时清除。' : '同组成员需使用相同密钥。', group: lb },
    { key: 'hc_kind', label: '健康检查', kind: 'select', options: [['', '关闭'], ['tcp', 'TCP'], ['http', 'HTTP']], value: health.kind ?? '', group: lb },
    { key: 'hc_path', label: '探测路径', value: health.path ?? '/', group: lb },
    { key: 'hc_timeout', label: '单次超时（秒）', kind: 'number', value: health.timeout_secs ?? 3, min: 1, max: 60, group: lb },
    { key: 'hc_failed', label: '连续失败阈值', kind: 'number', value: health.max_failed ?? 1, min: 1, max: 20, group: lb },
    { key: 'hc_interval', label: '探测间隔（秒）', kind: 'number', value: health.interval_secs ?? 10, min: 1, max: 3600, group: lb },
    { key: 'hc_headers', label: '探测请求头', kind: 'textarea', value: headerText(health.headers), group: lb },
    { key: 'plugin_action', label: '插件操作', kind: 'select', options: [['keep', '保持当前配置'], ['replace', '设置/替换完整配置'], ['remove', '移除插件']], value: 'keep', description: existing?.plugin ? `当前类型：${text(record(existing.plugin).kind)}。API 仅返回脱敏摘要。` : '当前未配置插件。', group: plugin },
    { key: 'plugin_json', label: '插件完整配置（JSON）', kind: 'json', description: `支持 ${pluginKinds.join(' / ')}。路径由客户端本机读取；*2* 插件仅支持 HTTP/1.1。替换需提供全部必要选项。`, maxBytes: 65536, group: plugin },
    { key: 'enabled', label: '启用隧道', kind: 'checkbox', value: source.enabled ?? true, group: common },
  ];
}
export function tunnelBody(values: RecordValue, existing?: Tunnel, clientId?: string): RecordValue {
  const kind = existing?.kind ?? text(values.kind, '');
  if (!tunnelKinds.includes(kind)) throw new Error('不支持的隧道类型');
  const domain = ['http', 'https', 'tcpmux'].includes(kind); const datagram = ['udp', 'sudp'].includes(kind); const balanced = ['tcp', 'http', 'https'].includes(kind);
  const body: RecordValue = {};
  for (const key of ['name', 'local_ip', 'local_port', 'enabled', 'max_connections', 'use_encryption', 'use_compression', 'reason']) body[key] = values[key];
  if (!existing) { body.kind = kind; body.client_id = clientId ?? values.client_id; }
  if (!domain && !['stcp', 'sudp'].includes(kind)) body.remote_port = values.remote_port;
  else if (!existing) body.remote_port = 0;
  if (domain) {
    const domains = Array.isArray(values.custom_domains) ? values.custom_domains : [];
    if (!domains.length && !text(values.subdomain, '')) throw new Error('请填写自定义域名或子域名');
    body.custom_domains = domains; body.subdomain = values.subdomain;
  }
  if (kind === 'http') {
    const locations = Array.isArray(values.locations) ? values.locations : [];
    if (locations.some(path => typeof path !== 'string' || !path.startsWith('/'))) throw new Error('路径前缀须以 / 开头');
    for (const key of ['http_user', 'route_by_http_user', 'host_header_rewrite']) body[key] = values[key];
    body.locations = locations; body.request_headers = headers(String(values.request_headers)); body.response_headers = headers(String(values.response_headers));
    if (values.clear_http_password) body.http_password = ''; else if (values.http_password !== undefined) body.http_password = values.http_password;
  }
  if (!datagram) { body.bandwidth_bps = values.bandwidth_bps; body.bandwidth_server = values.bandwidth_server; }
  if (kind === 'tcpmux') body.multiplexer = 'httpconnect';
  if (balanced) {
    body.proxy_protocol = Number(values.proxy_protocol); body.lb_group = values.lb_group;
    if (!values.lb_group) body.lb_group_key = ''; else if (values.lb_group_key !== undefined) body.lb_group_key = values.lb_group_key;
    const hc = text(values.hc_kind, '');
    if (hc === 'http' && !text(values.hc_path, '').startsWith('/')) throw new Error('健康检查路径须以 / 开头');
    if (hc) body.health_check = { kind: hc, timeout_secs: values.hc_timeout, max_failed: values.hc_failed, interval_secs: values.hc_interval, path: hc === 'http' ? values.hc_path : '', headers: hc === 'http' ? headers(String(values.hc_headers)) : {} };
    else if (existing) body.health_check = { kind: '', timeout_secs: 0, max_failed: 0, interval_secs: 0 };
  }
  if (values.plugin_action === 'remove') body.plugin = { kind: '' };
  else if (values.plugin_action === 'replace') {
    if (!['tcp', 'http', 'https'].includes(kind)) throw new Error('本地插件只能用于 TCP / HTTP / HTTPS 隧道');
    const plugin = record(values.plugin_json);
    if (!pluginKinds.includes(text(plugin.kind, ''))) throw new Error('请填写已支持的本地插件类型');
    body.plugin = plugin;
  }
  return body;
}
