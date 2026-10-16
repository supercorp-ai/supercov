// Every construct the JavaScript instrumenter rewrites, measured end to end.
//
//   node scripts/js-syntax-zoo.mjs
//
// A small project whose tests drive each construct both ways: loops with
// parts left out, negated compound decisions, logical assignment, optional
// calls and members (deleted through, too), rest patterns, decisions on a
// unary or a template, source read through a computed key or a string,
// sloppy `with` and direct `eval`, TypeScript `declare`, a shebang CLI, route
// handlers and a server entry. Each file is read back for what its tests
// showed: a construct measured wrongly reads as uncovered here.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-js-syntax-zoo-'));
const root = resolve(temporary, 'zoo');

const files = {
  'package.json': '{ "name": "zoo", "private": true, "type": "module" }\n',
  'src/flow.js': `export function firstNegative(values) {
  let index = 0;
  for (;;) {
    if (index >= values.length) return -1;
    if (values[index] < 0) return index;
    index += 1;
  }
}

export function countDown(n) {
  const seen = [];
  for (let i = n; ; i--) {
    if (i <= 0) break;
    seen.push(i);
  }
  return seen;
}

export function drain(queue) {
  let taken = 0;
  for (; queue.length > 0; ) {
    queue.pop();
    taken += 1;
  }
  return taken;
}

export function neither(a, b) {
  if (!(a || b)) return 'neither';
  return 'some';
}

export function notBoth(a, b) {
  return !(a && b) ? 'not both' : 'both';
}

export function until(items, stop) {
  let i = 0;
  while (!(i >= items.length || items[i] === stop)) i += 1;
  return i;
}
`,
  'src/values.js': `export function settings(given) {
  const result = { ...given };
  result.name ||= 'anonymous';
  result.count &&= result.count + 1;
  result.theme ??= 'light';
  return result;
}

export function forget(record) {
  delete record?.cache?.entry;
  return record;
}

export function call(target) {
  return target.run?.() ?? 'none';
}

export function callAt(target, key) {
  return target?.[key]?.() ?? 'none';
}

export function split({ id, ...rest }, [head, ...tail], ...more) {
  return { id, rest, head, tail, more };
}

export function sign(n) {
  return -n ? 'nonzero' : 'zero';
}

export function label(name) {
  return \`\${name}\` ? 'named' : 'empty';
}

export function pick(kind) {
  switch (kind) {
    case 'a':
      return 1;
    case 'b':
      return 2;
    default:
      return 0;
  }
}
`,
  'src/source.js': `export function plain(x) {
  return x + 1;
}

export function concatenated(x) {
  return x * 2;
}

export function stringified(x) {
  return x - 1;
}

export const keyed = { [plain]: true };
export const wrapped = '(' + concatenated + ')';
export const viaString = String(stringified);
`,
  'src/legacy.cjs': `function lookup(scope, name) {
  with (scope) {
    return eval(name);
  }
}

module.exports = { lookup };
`,
  'src/typed.ts': `declare module 'virtual:config' {
  export const value: number;
}

export class Box {
  declare tag: string;
  value: number;
  constructor(value: number) {
    this.value = value;
  }
}

export function scaled(value: number, factor: number = 2): number {
  return value * factor;
}

export const kind = (n: number): string => (n > 0 ? 'positive' : 'other');
`,
  'src/cli.js': `#!/usr/bin/env node
export function greet(name) {
  return name ? \`hello \${name}\` : 'hello';
}
`,
  'app/routes/items.ts': `export async function loader({ request }: { request: Request }) {
  const url = new URL(request.url);
  return url.searchParams.get('q') ?? 'all';
}

export async function action({ request }: { request: Request }) {
  return request.method === 'POST' ? 'created' : 'ignored';
}

export const title = 'Items';
`,
  'app/entry.server.ts': `export default function handleRequest(request: Request) {
  return request.method === 'HEAD' ? new Response(null) : new Response('ok');
}
`,
  'test/flow.test.mjs': `import { strict as assert } from 'node:assert';
import test from 'node:test';
import { countDown, drain, firstNegative, neither, notBoth, until } from '../src/flow.js';

test('loops with parts left out', () => {
  assert.equal(firstNegative([1, -2]), 1);
  assert.equal(firstNegative([1]), -1);
  assert.deepEqual(countDown(2), [2, 1]);
  assert.equal(drain([1, 2]), 2);
  assert.equal(drain([]), 0);
});

test('negated compound decisions', () => {
  assert.equal(neither(false, false), 'neither');
  assert.equal(neither(true, false), 'some');
  assert.equal(neither(false, true), 'some');
  assert.equal(notBoth(true, true), 'both');
  assert.equal(notBoth(true, false), 'not both');
  assert.equal(notBoth(false, true), 'not both');
  assert.equal(until(['a', 'b'], 'b'), 1);
  assert.equal(until(['a'], 'z'), 1);
  assert.equal(until([], 'z'), 0);
});
`,
  'test/values.test.cjs': `const assert = require('node:assert/strict');
const test = require('node:test');

test('values', async () => {
  const { settings, forget, call, callAt, split, sign, label, pick } = await import('../src/values.js');
  assert.deepEqual(settings({}), { name: 'anonymous', theme: 'light' });
  assert.deepEqual(settings({ name: 'a', count: 1, theme: 'dark' }), { name: 'a', count: 2, theme: 'dark' });
  assert.deepEqual(settings({ count: 0, theme: null }), { name: 'anonymous', count: 0, theme: 'light' });
  assert.deepEqual(forget({ cache: { entry: 1, kept: 2 } }), { cache: { kept: 2 } });
  assert.deepEqual(forget({}), {});
  assert.equal(forget(null), null);
  assert.equal(call({ run: () => 'ran' }), 'ran');
  assert.equal(call({}), 'none');
  assert.equal(callAt({ go: () => 'went' }, 'go'), 'went');
  assert.equal(callAt({}, 'go'), 'none');
  assert.equal(callAt(null, 'go'), 'none');
  assert.deepEqual(split({ id: 1, x: 2 }, [3, 4], 5), { id: 1, rest: { x: 2 }, head: 3, tail: [4], more: [5] });
  assert.equal(sign(3), 'nonzero');
  assert.equal(sign(0), 'zero');
  assert.equal(label('x'), 'named');
  assert.equal(label(''), 'empty');
  assert.deepEqual(['a', 'b', 'c'].map(pick), [1, 2, 0]);
});
`,
  'test/source.test.mjs': `import assert from 'node:assert';
import test from 'node:test';
import { keyed, plain, viaString, wrapped } from '../src/source.js';

test('source read three ways keeps its text', () => {
  assert.deepEqual(Object.keys(keyed), ['function plain(x) {\\n  return x + 1;\\n}']);
  assert.equal(wrapped, '(function concatenated(x) {\\n  return x * 2;\\n})');
  assert.equal(viaString, 'function stringified(x) {\\n  return x - 1;\\n}');
  assert.equal(plain(1), 2);
});
`,
  'test/legacy.test.cjs': `const { strict: assert } = require('node:assert');
const test = require('node:test');
const { lookup } = require('../src/legacy.cjs');

test('with and eval still read the scope', () => {
  assert.equal(lookup({ answer: 42 }, 'answer'), 42);
});
`,
  'test/typed.test.mjs': `import assert from 'node:assert/strict';
import test from 'node:test';
import { Box, kind, scaled } from '../src/typed.ts';
import { greet } from '../src/cli.js';

test('typed and shebang modules', () => {
  assert.equal(new Box(3).value, 3);
  assert.equal(scaled(2), 4);
  assert.equal(scaled(2, 3), 6);
  assert.equal(kind(1), 'positive');
  assert.equal(kind(0), 'other');
  assert.equal(greet('ann'), 'hello ann');
  assert.equal(greet(''), 'hello');
});
`,
  'test/routes.test.mjs': `import assert from 'node:assert/strict';
import test from 'node:test';
import { action, loader, title } from '../app/routes/items.ts';
import handleRequest from '../app/entry.server.ts';

test('route handlers and the server entry', async () => {
  assert.equal(await loader({ request: new Request('http://x/items?q=a') }), 'a');
  assert.equal(await loader({ request: new Request('http://x/items') }), 'all');
  assert.equal(await action({ request: new Request('http://x/items', { method: 'POST' }) }), 'created');
  assert.equal(await action({ request: new Request('http://x/items', { method: 'PUT' }) }), 'ignored');
  assert.equal(title, 'Items');
  assert.equal(await handleRequest(new Request('http://x/', { method: 'HEAD' })).text(), '');
  assert.equal(await handleRequest(new Request('http://x/')).text(), 'ok');
});
`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  run('git', ['init', '-q']);
  const plain = run(process.execPath, ['--test']);
  assert.equal(plain.status, 0, `${plain.stdout}${plain.stderr}`);
  const measured = run(binary, ['--', process.execPath, '--test']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const listed = JSON.parse(run(binary, ['runs', 'latest', 'files', '--json', '--limit', '100']).stdout).data.files;
  const byFile = Object.fromEntries(listed.map((entry) => [entry.file, entry]));
  const report = listed.map((entry) => `${entry.file}: lines ${entry.covered.lines}/${entry.totals.lines} branches ${entry.covered.branches}/${entry.totals.branches} MC/DC ${entry.covered.mcdcConditions}/${entry.totals.mcdcConditions} limits ${entry.measurementLimitations}`).join('\n');
  if (process.env.ZOO_REPORT) console.log(report);
  // Each construct was driven both ways, so nothing is left uncovered.
  for (const file of ['src/flow.js', 'src/values.js', 'src/typed.ts', 'src/cli.js', 'app/routes/items.ts', 'app/entry.server.ts']) {
    const entry = byFile[file];
    assert.ok(entry, `${file} was not measured\n${report}`);
    for (const metric of ['lines', 'branches', 'mcdcConditions']) {
      assert.equal(entry.covered[metric], entry.totals[metric], `${file}: ${metric}\n${report}`);
    }
    assert.ok(entry.totals.lines > 0, `${file} has no lines\n${report}`);
  }
  // Source that is read, and a scope `with` and `eval` reach into, are left
  // as written and say so.
  assert.ok(byFile['src/source.js'].measurementLimitations >= 3, report);
  assert.ok(byFile['src/legacy.cjs'].measurementLimitations >= 1, report);
  console.log('[js-syntax-zoo] every construct the JavaScript instrumenter rewrites reads as its tests drove it');
} finally {
  if (!process.env.KEEP) rmSync(temporary, { recursive: true, force: true });
}
