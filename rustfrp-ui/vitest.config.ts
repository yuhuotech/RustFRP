import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'jsdom',
    include: ['tests/**/*.test.ts'],
    restoreMocks: true,
    setupFiles: ['tests/setup.ts'],
    coverage: {
      provider: 'v8',
      include: ['src/**/*.{ts,js}'],
      reporter: ['text', 'json-summary', 'lcov', 'html'],
      thresholds: { lines: 90, statements: 90, functions: 90, branches: 90 },
    },
  },
});
