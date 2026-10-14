import { defineConfig, devices } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';

// Firefox 157 resolves macOS app data separately from -profile (upstream #42768).
// Keep automation state separate from the user's Firefox profile without widening permissions.
const firefoxState = resolve('../target/ui/firefox-state');
if (process.platform === 'darwin') mkdirSync(firefoxState, { recursive: true });
const firefoxLaunch = process.platform === 'darwin' ? { env: { ...process.env, CFFIXED_USER_HOME: firefoxState } } : {};

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 30000,
  reporter: [['list'], ['html', { open: 'never' }]],
  use: { baseURL: 'http://127.0.0.1:9674', trace: 'retain-on-failure', screenshot: 'only-on-failure', actionTimeout: 7000, navigationTimeout: 10000 },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'], viewport: { width: 1440, height: 900 } } },
    { name: 'firefox', use: { ...devices['Desktop Firefox'], viewport: { width: 1440, height: 900 }, launchOptions: firefoxLaunch } },
    { name: 'webkit', use: { ...devices['Desktop Safari'], viewport: { width: 1440, height: 900 } } },
  ],
});
