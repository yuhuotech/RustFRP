// Compiled only for the native-e2e application. Runs the shipped DOM UI in a real system WebView.
if (!window.__rustfrpNativeE2E) {
  window.__rustfrpNativeE2E = true;
  (async () => {
    const invoke = (command, args = {}) => window.__TAURI_INTERNALS__.invoke(command, args);
    const host = (action, args = {}) => invoke('host_command', { action, args });
    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
    const visibleFrame = async () => {
      const deadline = Date.now() + 10000;
      while (document.visibilityState !== 'visible') {
        if (Date.now() >= deadline) throw new Error('Native WebView did not become visible');
        await sleep(10);
      }
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    };
    const wait = async (condition, timeout = 30000) => {
      const deadline = Date.now() + timeout;
      while (Date.now() < deadline) { const result = await condition(); if (result) return result; await sleep(50); }
      throw new Error(`Native WebView condition timed out: ${String(condition).slice(0, 220)}`);
    };
    const bounded = async (operation, timeout, label) => {
      let timer;
      try {
        return await Promise.race([
          operation,
          new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${label} timed out`)), timeout); }),
        ]);
      } finally {
        clearTimeout(timer);
      }
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
      const trayPolicy = await invoke('native_test_tray_policy');
      assert(trayPolicy.show && trayPolicy.exit && trayPolicy.unknown && trayPolicy.left_release && !trayPolicy.right_release, 'Tray actions do not follow the native policy');
      if (['isolated-startup', 'home-startup', 'no-tray-startup'].includes(config.mode)) {
        const info = await host('info');
        const preferences = await host('preferences');
        assert(info.desktop && info.local_defaults, 'Native startup capabilities are missing');
        assert((await host('startup')) === false, 'Fresh isolated startup unexpectedly enabled');
        assert(!Object.hasOwn(preferences, 'password') && !Object.hasOwn(preferences, 'token'), 'Startup preferences exposed a credential');
        if (config.mode === 'no-tray-startup') assert(await invoke('native_window_visibility'), 'The window stayed hidden when the tray was unavailable');
        await report({ stage: config.mode, passed: true, isolated_preferences: true, startup_disabled: true });
        return;
      }
      if (localStorage.getItem('native-e2e.run') !== config.name) { localStorage.setItem('native-e2e.run', config.name); localStorage.removeItem('native-e2e.phase'); evidence = {}; }
      if (localStorage.getItem('native-e2e.phase') === 'release') {
        await wait(() => document.querySelector('[data-testid="page-title"]')?.textContent === '客户端');
        const me = await api('me'); assert(me.admin.name === 'preview', 'Native session was lost on WebView restoration');
        const local = await host('localStatus'); assert(local.connected, 'Agent stopped after WebView release');
        assert(local.pid === Number(localStorage.getItem('native-e2e.agent-pid')), 'Window release replaced the background Agent');
        await report({ stage: 'restored', passed: true, session_restored: true, page_restored: true, agent_survives_release: true, agent_pid: local.pid });
        return;
      }
      await wait(() => document.querySelector('[data-testid="local-start"]'));
      assert(document.querySelector('[data-testid="local-start"]').textContent === '登录并连接此电脑', 'Saved address was mistaken for enrollment');
      click('local-start'); await wait(() => document.querySelector('#login-name'));
      field('#login-server', config.controller); field('#login-name', 'preview'); field('#login-password', config.password);
      click('login-submit'); await wait(() => document.querySelector('dialog select[name="node"]'));
      assert(!document.querySelector('dialog input[name="enrollToken"]'), 'Login continuation asked the user for a token');
      click('dialog-cancel'); navigation('overview'); await wait(() => document.querySelectorAll('.metric-card').length === 6);
      evidence.saved_settings_login = true;
      assert(!document.cookie.includes('rfrp_session'), 'Cookie exposed to JavaScript');
      assert(!(await api('me')).csrf, 'CSRF exposed to JavaScript');
      const rejected = async operation => { try { await operation(); } catch (error) { assert(error?.code === 'invalid_request', 'Unexpected native rejection'); return; } throw new Error('Native boundary accepted an invalid operation'); };
      await rejected(() => invoke('native_test_report', { result: { stage: 'raw' } }));
      await rejected(() => invoke('native_test_report', { result: { stage: 'failed', payload: 'x'.repeat(5000) } }));
      await rejected(() => invoke('native_window_cycle', { delayMs: 51001 }));
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
      assert((await host('startup')) === false, 'Isolated startup unexpectedly enabled');
      assert((await host('setStartup', { enabled: true })) === true && (await host('startup')) === true, 'Native login startup did not enable');
      assert((await host('setStartup', { enabled: false })) === false && (await host('startup')) === false, 'Native login startup did not disable');
      evidence.startup_roundtrip = true;
      for (const section of ['nodes', 'clients', 'tunnels', 'settings', 'overview']) { navigation(section); await sleep(75); assert(document.querySelector('[data-testid="page-title"]'), 'Navigation lost the real page'); }
      navigation('local'); await wait(() => {const button=document.querySelector('[data-testid="local-start"]'); return button && !button.disabled;});
      assert(document.querySelector('[data-testid="local-start"]').textContent === '连接此电脑', 'Fresh setup did not offer guided connection');
      click('local-start'); await wait(() => document.querySelector('dialog select[name="node"]'));
      assert(!document.querySelector('dialog input[name="controller"]') && !document.querySelector('dialog input[name="binary"]'), 'Guided setup requires technical paths or addresses');
      field('dialog input[name="name"]', config.name); submit(); await wait(() => !document.querySelector('dialog[open]'));
      let local = await wait(async () => { const status = await host('localStatus'); return status?.connected ? status : null; });
      assert((await host('info')).local_enrolled === true, 'Actual enrollment was not reflected in capabilities');
      const credentialCount = (await api('enrollTokens')).length;
      click('local-stop'); await wait(() => document.querySelector('dialog[open]')); submit(); await wait(() => !document.querySelector('dialog[open]'));
      await wait(() => document.querySelector('[data-testid="local-start"]')?.textContent === '启动连接');
      click('local-start'); assert(!document.querySelector('dialog[open]'), 'Restart asked for an enrollment token');
      local = await wait(async () => { const status = await host('localStatus'); return status?.connected ? status : null; });
      assert((await api('enrollTokens')).length === credentialCount, 'Restart created another enrollment credential');
      evidence.restart_without_token = true;
      const saved = await host('preferences'); const nodes = await api('nodes');
      assert(saved.local.tlsPin === nodes[0].tls_fingerprint, 'Guided setup did not use the Controller certificate');
      assert(!JSON.stringify(saved).includes(config.token), 'Enrollment credential entered preferences');
      const enrollments = await api('enrollTokens');
      assert(enrollments.some(item => item.name === config.name && item.used_at != null), 'Guided enrollment did not consume its credential');
      evidence.guided_connection = true;
      localStorage.setItem('native-e2e.agent-pid', String(local.pid));
      await report({ stage: 'local-started', passed: true, agent_pid: local.pid, login: true, six_metrics: true, native_session_private: true });
      await sleep(1500);
      await report({ stage: 'window-cycle-start', passed: true, cycles: 0 });
      const cycleTimings = [];
      for (let cycle = 0; cycle < 30; cycle++) {
        const started = performance.now();
        await bounded(invoke('native_window_cycle', { delayMs: 25 }), 15000, `Native window cycle ${cycle + 1}/30`);
        await bounded(visibleFrame(), 15000, `Native visible frame ${cycle + 1}/30`);
        cycleTimings.push(performance.now() - started);
        assert(await invoke('native_window_visibility'), 'Native WebView is not visible after its first frame');
        if (cycle === 0 || cycle === 29) await report({ stage: 'window-cycles', passed: true, cycles: cycle + 1 });
      }
      const afterCycles = await bounded(host('localStatus'), 15000, 'Agent status after window cycles');
      assert(afterCycles.pid === local.pid, 'Warm window cycles changed Agent PID');
      const p95 = values => [...values].sort((a, b) => a - b)[Math.ceil(values.length * 0.95) - 1];
      await report({ stage: 'warm-cycles', passed: true, cycles: 30, visible_first_frame_p95_ms: p95(cycleTimings), agent_pid: local.pid });
      await host('window', { action: 'minimize' });
      await host('window', { action: 'show' });
      await host('window', { action: 'close' });
      await sleep(100);
      await host('window', { action: 'show' });
      assert((await host('localStatus')).pid === local.pid, 'Close-to-tray replaced the background Agent');
      navigation('settings'); await wait(() => document.querySelector('[data-testid="setting-api"]')); click('setting-api'); await wait(() => document.querySelector('[data-testid="new-api-token"]'));
      click('new-api-token'); field('dialog input[name="name"]', config.name); field('dialog input[name="reason"]', 'Native credentials release guard E2E'); submit();
      await wait(() => document.querySelector('dialog[data-sensitive="true"] textarea'));
      await invoke('native_window_cycle', { delayMs: 51000 });
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
