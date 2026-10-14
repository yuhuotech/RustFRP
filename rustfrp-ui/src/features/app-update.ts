import type { Context } from '../app/context';
import { record, text, type RecordValue } from '../api/models';
import { button, details, el, panel } from '../components/dom';
import { FormDialog } from '../components/forms';
import { guarded, View } from '../components/operations';

export class AppUpdate {
  readonly element = panel('应用更新', 'transfer');
  private view = new View(this.element);
  private state: RecordValue = {};
  private source = 'github';
  private pending = false;
  private disposed = false;
  private timer?: ReturnType<typeof setTimeout>;
  constructor(private readonly context: Context) {}
  async refresh(): Promise<void> {
    const [state, preferences] = await Promise.all([this.context.host.call('updateStatus'), this.context.host.call('preferences')]);
    if (this.disposed) return;
    this.state = record(state); this.source = record(preferences).update_source === 'mainland' ? 'mainland' : 'github'; this.paint();
  }
  private paint(): void {
    const state = this.state, release = record(state.latest ?? {}), busy = this.pending || ['checking', 'downloading', 'installing'].includes(text(state.phase));
    this.view.update([state, this.source, this.pending], () => {
      const source = el('select'); source.setAttribute('aria-label', '应用更新源');
      for (const [value, label] of [['github', 'GitHub 官方'], ['mainland', '国内镜像']]) { const option = el('option', '', label); option.value = value; source.append(option); }
      source.value = this.source; source.disabled = busy;
      source.addEventListener('change', () => { const previous = this.source; source.disabled = true; void this.context.host.call('savePreferences', { update_source: source.value }).then(() => this.refresh()).catch(error => { source.value = previous; this.context.showError(error); }).finally(() => { source.disabled = false; }); });
      const status = state.disabled ? text(state.disabled) : state.error ? text(state.error) : state.phase === 'checking' ? '正在检查更新…' : state.phase === 'downloading' ? '正在下载并校验更新…' : state.phase === 'installing' ? '安装程序已接续，应用即将重新启动。' : state.phase === 'cancelled' ? '更新已取消' : state.available === true ? `发现新版本 ${text(release.version)}` : release.version ? '当前已是最新稳定版本' : '检查最新稳定版本';
      const message = el('p', state.error || state.disabled ? 'warning-text' : 'muted-text', status); message.setAttribute('role', 'status');
      const progress = el('progress'); const total = Number(state.total), count = Number(state.downloaded);
      progress.max = Number.isFinite(total) && total > 0 ? total : 1; progress.value = Number.isFinite(count) && count >= 0 ? count : 0; progress.hidden = state.phase !== 'downloading'; progress.setAttribute('aria-label', '更新下载进度');
      const actions = el('div', 'actions', button('检查更新', guarded(this.context, () => this.run('checkUpdate')), { id: 'app-update-check', disabled: busy || !!state.disabled }));
      if (state.available === true && !state.manual && !state.disabled) actions.append(button('安装并重启', () => this.confirm(), { primary: true, id: 'app-update-install', disabled: busy }));
      if (['checking', 'downloading'].includes(text(state.phase)) || this.pending) actions.append(button('取消更新', guarded(this.context, async () => { await this.context.host.call('cancelUpdate'); await this.refresh(); }), { id: 'app-update-cancel' }));
      if (release.page) actions.append(button('发布详情', guarded(this.context, () => this.context.host.call('openUrl', { url: text(release.page) })), { id: 'app-update-release' }));
      return [el('h3', 'panel-heading', '应用更新'), source, message, state.manual ? el('p', 'warning-text', text(state.manual)) : null, details([['更新源', this.source === 'mainland' ? '国内镜像' : 'GitHub 官方'], ['当前连接', state.mirror], ['最新版本', release.version]]), progress, actions, release.notes ? el('pre', 'text-preview', text(release.notes)) : null, el('p', 'field-help', '安装前验证 Ed25519 签名、版本标记和 SHA256。更新客户端不会停止后台 Agent。')].filter((child): child is HTMLElement => child !== null);
    });
  }
  private confirm(): void {
    const release = record(this.state.latest ?? {});
    const dialog = new FormDialog('安装应用更新', [], async (_, current) => { await this.run('installUpdate'); current.finish(); }, '安装并重启');
    dialog.content.append(el('p', '', `将安装 ${text(release.version)}。应用窗口会退出并重新启动，后台 Agent 继续运行。`));
  }
  private async run(action: 'checkUpdate' | 'installUpdate'): Promise<void> {
    if (this.pending) return;
    this.pending = true; this.paint();
    const poll = () => { this.timer = setTimeout(() => { void this.refresh().catch(this.context.showError).finally(() => { if (this.pending && !this.disposed) poll(); }); }, 500); }; poll();
    try { const state = await this.context.host.call(action); if (!this.disposed) this.state = record(state); }
    finally { this.pending = false; clearTimeout(this.timer); if (!this.disposed) { this.paint(); await this.refresh(); } }
  }
  dispose(): void { this.disposed = true; clearTimeout(this.timer); }
}
