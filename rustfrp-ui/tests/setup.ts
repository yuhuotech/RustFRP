import { beforeEach, vi } from 'vitest';

Object.defineProperty(globalThis, 'matchMedia', { writable: true, value: vi.fn(() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) });
Object.defineProperty(globalThis.CSS, 'escape', { value: (value: string) => value.replace(/[^a-zA-Z0-9_-]/g, '\\$&'), configurable: true });
HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); this.dispatchEvent(new Event('close')); };
beforeEach(() => { document.body.innerHTML = '<div id="app"></div><div id="notifications"></div>'; localStorage.clear(); document.documentElement.removeAttribute('data-theme'); document.documentElement.removeAttribute('data-collapsed'); });
