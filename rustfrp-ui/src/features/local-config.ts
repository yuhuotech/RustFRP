import type { Context } from '../app/context';
import { record, text, type RecordValue } from '../api/models';
import { button, el, toast } from '../components/dom';
import { FormDialog, type Field } from '../components/forms';

export async function localSettings(context: Context): Promise<void> {
  const focus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const preferences = record(await context.host.call('preferences')); const config = preferences.local ? record(preferences.local) : {};
  const info = record(await context.host.call('info')); const defaults = info.local_defaults ? record(info.local_defaults) : {};
  const fields: Field[] = [
    { key: 'mode', label: '连接方式', kind: 'select', value: config.configFile ? 'standalone' : 'managed', options: [['managed', '接入管理服务'], ['standalone', '使用已有 frpc 配置文件']] },
    { key: 'controller', label: '接入地址', value: config.controller ?? '', description: '向管理员获取，例如 frp.example.com:7000。登录管理服务后可自动填写。', group: '受管连接' },
    { key: 'name', label: '这台电脑的名称', value: config.name ?? defaults.name ?? '本机', maxLength: 64, group: '受管连接' },
    { key: 'configFile', label: 'frpc 配置文件', value: config.configFile ?? '', description: '选择已有的 TOML、YAML 或 JSON 文件，之后可在应用内编辑。', group: '独立配置' },
    { key: 'caFile', label: '可信 CA 文件', value: config.caFile ?? '', description: '仅在管理员提供专用证书时填写绝对路径。', group: '证书设置' },
    { key: 'tlsPin', label: '证书 SHA-256 指纹', value: config.tlsPin ?? '', maxLength: 64, group: '证书设置' },
    { key: 'binary', label: 'Agent 程序', value: config.binary ?? defaults.binary ?? '', required: true, description: '已自动选择随应用安装的程序。', group: '本机文件' },
    { key: 'stateDir', label: '身份与状态目录', value: config.stateDir ?? defaults.stateDir ?? '', required: true, description: '已自动设置。更换管理服务时使用独立目录。', group: '本机文件' },
  ];
  const dialog = new FormDialog('本机连接设置', fields, async (values, current) => {
    const local: RecordValue = { ...config, binary: values.binary, stateDir: values.stateDir };
    for (const key of ['controller', 'name', 'caFile', 'tlsPin']) local[key] = values.mode === 'managed' ? values[key] : config[key] ?? '';
    local.configFile = values.mode === 'standalone' ? values.configFile : '';
    if (values.mode === 'managed' && !text(local.controller, '')) throw new Error('请填写管理员提供的接入地址，或登录管理服务自动配置。');
    if (values.mode === 'standalone' && !text(local.configFile, '')) throw new Error('请填写独立配置文件路径。');
    await context.host.call('savePreferences', { local }); current.finish(); toast('本机连接设置已保存'); await context.refresh();
  }, '保存', focus);
  dialog.element.classList.add('connection-dialog');
  const advanced = el('details', 'connection-advanced', el('summary', '', '高级设置（通常无需修改）'));
  for (const name of ['证书设置', '本机文件']) {
    const group = [...dialog.content.querySelectorAll('fieldset')].find(group => group.querySelector('legend')?.textContent === name);
    if (group) advanced.append(group);
  }
  dialog.content.append(advanced);
  if (!context.admin) dialog.content.prepend(el('div', 'connection-guide', el('strong', '', '推荐：登录后自动接入'), el('p', 'muted-text', '选择服务端节点即可，地址和证书由应用填写。'), button('登录管理服务', () => { dialog.close(); return context.logout(); }, { id: 'local-login' })));
  const file = dialog.control('configFile');
  file?.closest('.form-field')?.append(button('选择文件…', async () => {
    const selected = await context.host.call('pickFile', { kind: 'config' });
    if (typeof selected === 'string') dialog.set('configFile', selected);
  }, { id: 'local-pick-config' }));
  const update = () => { const standalone = dialog.control('mode')?.value === 'standalone'; dialog.showGroup('受管连接', !standalone); dialog.showGroup('独立配置', standalone); dialog.showGroup('证书设置', !standalone); };
  dialog.control('mode')?.addEventListener('change', update); update();
}
export async function localConfigEditor(context: Context): Promise<FormDialog> {
  const focus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const current = record(await context.host.call('readConfig'));
  let report: RecordValue | undefined; let verifiedText = ''; let checking = false; let closed = false;
  const summary = el('div');
  const dialog = new FormDialog('编辑完整 frpc 配置', [
    { key: 'text', label: 'TOML / YAML / JSON 配置文本', kind: 'textarea', value: current.text, required: true, maxBytes: 1024 * 1024, description: '保留完整配置字段、includes 与环境模板。校验通过后以私有文件原子保存。' },
    { key: 'accept_warnings', label: '已检查兼容报告，确认保存当前配置', kind: 'checkbox', value: false },
  ], async (values, form) => {
    if (!report || verifiedText !== values.text || values.accept_warnings !== true) throw new Error('配置已变化，请重新校验并确认兼容报告。');
    await context.host.call('saveConfig', { text: values.text, original_digest: current.digest ?? '', preview_digest: report.digest, accept_warnings: true });
    form.finish(); toast('配置已安全保存。重新启动独立 Agent 后生效。');
  }, '保存', focus);
  dialog.element.dataset.sensitive = 'true';
  const input = dialog.control('text'); const acknowledgment = dialog.control('accept_warnings');
  const eligibility = () => { dialog.submit.disabled = checking || dialog.busy || !report || input?.value !== verifiedText || !(acknowledgment instanceof HTMLInputElement && acknowledgment.checked) || dialog.submit.dataset.uncertain === 'true'; };
  const verify = button('校验配置', async () => {
    if (checking || dialog.busy) return; checking = true; report = undefined; verify.disabled = true; verify.setAttribute('aria-busy', 'true'); dialog.error.hidden = true; dialog.set('accept_warnings', false); eligibility();
    const source = input?.value ?? '';
    try {
      if (!source.trim() || new TextEncoder().encode(source).length > 1024 * 1024) throw new Error('配置不能为空且不能超过 1 MiB。');
      const value = record(await context.host.call('verifyConfig', { text: source }));
      if (closed || input?.value !== source) return;
      if (typeof value.digest !== 'string' || !Array.isArray(value.warnings)) throw new Error('配置校验响应不兼容，请更新桌面客户端。');
      report = value; verifiedText = source;
      summary.replaceChildren(el('h3', '', `校验通过 · ${value.proxies} 条代理 / ${value.visitors} 个访问者`), ...value.warnings.map(warning => el('p', 'warning-text', String(warning))), ...(value.warnings.length ? [] : [el('p', 'muted-text', '未发现兼容性警告。')]));
      dialog.set('accept_warnings', value.warnings.length === 0);
    } catch (error) { if (!closed) { dialog.error.textContent = error instanceof Error ? error.message : String(error); dialog.error.hidden = false; } }
    finally { checking = false; verify.disabled = false; verify.setAttribute('aria-busy', 'false'); eligibility(); }
  }, { id: 'local-config-verify' });
  const anchor = acknowledgment?.closest('.form-field'); dialog.content.insertBefore(verify, anchor ?? null); dialog.content.insertBefore(summary, anchor ?? null);
  input?.addEventListener('input', () => { report = undefined; dialog.set('accept_warnings', false); summary.replaceChildren(el('p', 'muted-text', '配置已修改，请重新校验。')); eligibility(); }); acknowledgment?.addEventListener('change', eligibility);
  dialog.onClose = () => { closed = true; verifiedText = ''; current.text = ''; report = undefined; summary.replaceChildren(); }; eligibility(); return dialog;
}
