// The upstream config's "main" project alone (src/**/*.test.*), in Node, without
// coverage (upstream enables v8 coverage by default) and without the runtime-test
// projects (workerd, fastly, lambda, ...), which need other runtimes.
import { configDefaults, defineConfig } from 'vitest/config'

export default defineConfig({
  oxc: {
    jsx: {
      runtime: 'automatic',
      importSource: './src/jsx',
    },
  },
  test: {
    globals: true,
    include: ['src/**/(*.)+(spec|test).+(ts|tsx|js)'],
    exclude: [...configDefaults.exclude, '**/sandbox/**', '**/*.case.test.*'],
  },
})
