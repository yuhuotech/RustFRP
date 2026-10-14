import { App } from './app/app';

const root = document.getElementById('app');
if (root) {
  const app = new App(root);
  void app.start();
  addEventListener('pagehide', () => app.dispose(), { once: true });
}
