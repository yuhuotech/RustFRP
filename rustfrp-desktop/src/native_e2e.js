// Compiled only for the native-e2e application. Runs the shipped DOM UI in a real system WebView.
if (!window.__rustfrpNativeE2E) {
  window.__rustfrpNativeE2E = true;
  (async () => {
    const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(command, args);
    const host = (action, args = {}) => invoke('host_command', { action, args });
    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
    const wait = async (condition, timeout = 30000) => {
      const deadline = Date.now() + timeout;
      while (Date.now() < deadline) { const result = await condition(); if (result) return result; await sleep(50); }
      throw new Error('Native WebView condition timed out');
    };
    const assert = (condition, message) => { if (!condition) throw new Error(message); };
    const click = id => { const control = document.querySelector(`[data-testid="${id}"]`); assert(control && !control.disabled, `Control unavailable: ${id}`); control.click(); };
    const field = (selector, value) => { const input = document.querySelector(selector); assert(input, `Missing field: ${selector}`); input.value = value; input.dispatchEvent(new Event('input', { bubbles: true })); };
    const navigation = section => document.querySelector(`.sidebar [data-section="${section}"]`).click();
    const submit = () => document.querySelector('dialog form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    const api = (operation, args = {}) => invoke('controller_request', { operation, args, requestId: crypto.randomUUID() });
    let evidence = JSON.parse(localStorage.getItem('native-e2e.evidence') ?? '{}');
    const report = result => { evidence = { ...evidence, ...result }; localStorage.setItem('native-e2e.evidence', JSON.stringify(evidence)); return invoke('native_test_report', { result: evidence }); };
    try {
      const config = await invoke('native_test_config');
      if (localStorage.getItem('native-e2e.run') !== config.name) { localStorage.setItem('native-e2e.run', config.name); localStorage.removeItem('native-e2e.phase'); evidence = {}; }
      if (localStorage.getItem('native-e2e.phase') === 'release') {
        await wait(() => document.querySelector('[data-testid="page-title"]')?.textContent === '客户端');
        const me = await api('me'); assert(me.admin.name === 'preview', 'Native session was lost on WebView restoration');
        const local = await host('localStatus'); assert(local.connected, 'Agent stopped after WebView release');
        assert(local.pid === Number(localStorage.getItem('native-e2e.agent-pid')), 'Window release replaced the background Agent');
        await report({ stage: 'restored', passed: true, session_restored: true, page_restored: true, agent_survives_release: true, agent_pid: local.pid });
        return;
      }
      await wait(() => document.querySelector('#login-name'));
      field('#login-server', config.controller); field('#login-name', 'preview'); field('#login-password', config.password);
      click('login-submit'); await wait(() => document.querySelectorAll('.metric-card').length === 6);
      assert(!document.cookie.includes('rfrp_session'), 'Cookie exposed to JavaScript');
      assert(!(await api('me')).csrf, 'CSRF exposed to JavaScript');
      const rejected = async operation => { try { await operation(); } catch (error) { assert(error?.code === 'invalid_request', 'Unexpected native rejection'); return; } throw new Error('Native boundary accepted an invalid operation'); };
      for (const action of ['copy', 'openUrl', 'window', 'windowState', 'setNotifications']) {
        await rejected(() => host(action, { text: false, url: 'file:///etc/passwd', action: 'raw', enabled: 'true' }));
      }
      await rejected(() => host('copy', { text: 'x'.repeat(1024 * 1024 + 1) }));
      await rejected(() => host('copy', []));
      await rejected(() => host('pickFile', { kind: 'unknown' }));
      await rejected(() => host('rawShell', { command: 'echo unsafe' }));
      await rejected(() => host('selectController', { url: 'https://user:password@example.com' }));
      assert((await api('me')).admin.name === 'preview', 'Invalid controller selection discarded a valid session');
      await rejected(() => api('rawHttp', { body: {} }));
      const cancelId = crypto.randomUUID();
      assert((await invoke('controller_cancel', { requestId: cancelId })).cancelled === false, 'Cancellation claimed an active request before dispatch');
      try { await invoke('controller_request', { operation: 'me', args: {}, requestId: cancelId }); throw new Error('Early cancellation was ignored'); } catch (error) { assert(error?.code === 'cancelled' && !error.uncertain, 'Read cancellation has incorrect certainty'); }
      const info = await host('info'); assert(info.desktop && info.version && info.local_defaults, 'Native capabilities are missing');
      const notification = await host('notifications'); assert(notification.enabled === false && notification.system_managed, 'Native notification settings diverged');
      assert((await host('setNotifications', { enabled: false })).enabled === false, 'Disabled notifications did not persist');
      const update = await host('updateStatus');
      if (update.disabled) { await rejected(() => host('checkUpdate')); await rejected(() => host('installUpdate')); }
      await host('cancelUpdate');
      for (const section of ['nodes', 'clients', 'tunnels', 'settings', 'overview']) { navigation(section); await sleep(75); assert(document.querySelector('[data-testid="page-title"]'), 'Navigation lost the real page'); }
      navigation('local'); await wait(() => {const button=document.querySelector('[data-testid="local-start"]'); return button && !button.disabled;});
      click('local-start'); await wait(() => document.querySelector('input[name="enrollToken"]'));
      field('input[name="enrollToken"]', config.token); submit(); await wait(() => !document.querySelector('dialog[open]'));
      const local = await wait(async () => { const status = await host('localStatus'); return status?.connected ? status : null; });
      localStorage.setItem('native-e2e.agent-pid', String(local.pid));
      await report({ stage: 'local-started', passed: true, agent_pid: local.pid, login: true, six_metrics: true, native_session_private: true });
      await sleep(1500);
      const timings = [];
      for (let cycle = 0; cycle < 30; cycle++) { await host('window', { action: 'hide' }); const started = performance.now(); await host('window', { action: 'show' }); await new Promise(resolve => requestAnimationFrame(resolve)); timings.push(performance.now() - started); }
      assert((await host('localStatus')).pid === local.pid, 'Warm window cycles changed Agent PID'); timings.sort((a, b) => a - b);
      await report({ stage: 'warm-cycles', passed: true, cycles: 30, warm_open_p95_ms: timings[28], agent_pid: local.pid });
      navigation('settings'); await wait(() => document.querySelector('[data-testid="setting-api"]')); click('setting-api'); await wait(() => document.querySelector('[data-testid="new-api-token"]'));
      click('new-api-token'); field('dialog input[name="name"]', config.name); field('dialog input[name="reason"]', 'Native credentials release guard E2E'); submit();
      await wait(() => document.querySelector('dialog[data-sensitive="true"] textarea'));
      await host('window', { action: 'hide' }); await sleep(51000);
      assert(document.querySelector('dialog[data-sensitive="true"] textarea')?.value, 'One-time credential dialog was destroyed');
      await host('window', { action: 'show' }); click('dialog-submit'); await wait(() => !document.querySelector('dialog'));
      const tokens = await api('apiTokens'); const token = tokens.find(item => item.name === config.name); assert(token, 'Test token missing');
      await api('revokeApiToken', { id: token.id, body: { reason: 'Native E2E cleanup' } });
      await report({ stage: 'sensitive-protected', passed: true, one_time_credential_preserved: true, credential_cleanup: true });
      navigation('clients'); await sleep(100); await host('savePreferences', { local_mode: false });
      localStorage.setItem('native-e2e.phase', 'release'); await report({ stage: 'release', passed: true }); await host('window', { action: 'hide' });
    } catch (error) { await report({ stage: 'failed', passed: false, message: error.message ?? String(error) }); }
  })();
}
