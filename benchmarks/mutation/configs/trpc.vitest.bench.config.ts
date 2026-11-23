// The server-side suites: packages/tests/server (integration tests that start
// HTTP, WebSocket and SSE servers and call them through @trpc/client) and the
// unit tests next to @trpc/server's sources. React suites are left out. Shared
// settings (source aliases, jsdom, setup file) come from the upstream config;
// retries are off (upstream retries twice when CI is set). The pool is named
// explicitly: upstream relies on Vitest's default (forks) for --expose-gc, which
// worker threads reject. (StrykerJS picks its own pool: see corpus.json.)
import { defineConfig, mergeConfig } from 'vitest/config';
import shared from './vitest.shared.ts';

export default mergeConfig(
  shared,
  defineConfig({
    test: {
      include: [
        'packages/tests/server/**/*.test.ts',
        'packages/server/src/**/*.test.ts',
      ],
      retry: 0,
      pool: 'forks',
    },
  }),
);
