import type { Host } from './host';

/** The host receives booleans only; no input, cookie, config or credential crosses this path. */
export function observeWindow(host: Host): () => void {
  if (!host.desktop) return () => {};
  let previous = ''; let pending = false; let dirty = false; let disposed = false;
  const report = () => {
    pending = false; if (disposed) return;
    const state = {
      busy: !!document.querySelector('[aria-busy="true"], dialog.busy'),
      dirty: dirty && !!document.querySelector('dialog[open]'),
      sensitive: !!document.querySelector('dialog[open]') || [...document.querySelectorAll<HTMLInputElement>('input[type="password"]')].some(input => !!input.value),
    };
    const signature = JSON.stringify(state); if (signature === previous) return;
    previous = signature; void host.call('windowState', state).catch(() => { previous = ''; });
  };
  const schedule = () => { if (!pending) { pending = true; queueMicrotask(report); } };
  const input = () => { dirty = true; schedule(); };
  const observer = new MutationObserver(schedule);
  observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ['open', 'aria-busy', 'data-sensitive', 'class'] });
  document.addEventListener('input', input); document.addEventListener('visibilitychange', schedule); report();
  return () => { disposed = true; observer.disconnect(); document.removeEventListener('input', input); document.removeEventListener('visibilitychange', schedule); };
}
