// Every construct the JavaScript instrumenter rewrites, measured end to end.
//
//   node scripts/js-syntax-zoo.mjs
//
// A small project whose tests drive each construct both ways: loops with
// parts left out, negated compound decisions, logical assignment, optional
// calls and members (deleted through, too, and called by a computed key),
// parenthesized logic, rest patterns, decisions on a unary or a template,
// source read through a computed key or a string,
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

export function fallback(a, b) {
  if (!(a ?? b)) return 'nothing';
  return 'something';
}

export function either(a, b) {
  return (a ?? b) ? 'something' : 'falsy';
}

export function both(a, b) {
  if (!!(a && b)) return 'both';
  return 'not both';
}

export function claim(slot, value) {
  if ((slot.value ||= value)) return slot.value;
  return 'empty';
}

export function shape(o) {
  return [typeof o?.a, void o?.a, !o?.a];
}

export function scan(limit, step) {
  let seen = 0;
  for (let i = 0; i < limit && (step ?? 1) > 0; ) {
    i += step ?? 1;
    seen += 1;
  }
  return seen;
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

export function reflective(o, x, y) {
  let v = x;
  v ||= y;
  const w = o?.f?.();
  switch (v) {
    case 1:
      return w;
    default:
      return x && y;
  }
}

export const reflectiveText = String(reflective);
export const shelf = { label(name) { return name ? name.toUpperCase() : 'none'; } };
export const shelfText = shelf.label.toString();
export const keyed = { [plain]: true };
export const wrapped = '(' + concatenated + ')';
export const viaString = String(stringified);
`,
  'src/legacy.cjs': `function lookup(scope, name) {
  with (scope) {
    return eval(name);
  }
}

function inside(scope) {
  with (scope) {
    switch (kind) {
      case 'call':
        return target?.go?.() ?? (left && right);
      default:
        return (left ||= right);
    }
  }
}

module.exports = { lookup, inside };
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

export function helper() {
  return 'help';
}

export let headers;
headers = () => ({ 'cache-control': 'no-store' });

const page = 'items';
export { page };

export default function ItemsPage() {
  return 'page';
}
`,
  'app/routes/shared.ts': `export async function sharedLoader() {
  return 'shared';
}

export function extra() {
  return 'extra';
}
`,
  'app/routes/reexport.ts': `export { sharedLoader as loader, extra } from './shared.ts';
export { extra as 'extra-name' } from './shared.ts';
export { extra as unrelated } from './shared.ts';
`,
  'app/entry.server.ts': `export const streamTimeout = 5000;

function handleRequest(request: Request) {
  return request.method === 'HEAD' ? new Response(null) : new Response('ok');
}

export default handleRequest;
`,
  'test/flow.test.mjs': `import { strict as assert } from 'node:assert';
import test from 'node:test';
import { both, claim, countDown, drain, either, fallback, firstNegative, neither, notBoth, scan, shape, until } from '../src/flow.js';

test('loops with parts left out', () => {
  assert.equal(firstNegative([1, -2]), 1);
  assert.equal(firstNegative([1]), -1);
  assert.deepEqual(countDown(2), [2, 1]);
  assert.equal(drain([1, 2]), 2);
  assert.equal(drain([]), 0);
});

test('coalescing and assigning decisions', () => {
  assert.equal(fallback(null, 0), 'nothing');
  assert.equal(fallback(null, 1), 'something');
  assert.equal(fallback(2, null), 'something');
  assert.equal(either(null, 0), 'falsy');
  assert.equal(either(1, null), 'something');
  assert.equal(both(true, true), 'both');
  assert.equal(both(true, false), 'not both');
  assert.equal(both(false, true), 'not both');
  assert.equal(claim({}, 'x'), 'x');
  assert.equal(claim({ value: 'y' }, 'x'), 'y');
  assert.equal(claim({}, ''), 'empty');
  assert.deepEqual(shape({ a: 1 }), ['number', undefined, false]);
  assert.deepEqual(shape(null), ['undefined', undefined, true]);
  assert.equal(scan(2, 1), 2);
  assert.equal(scan(3, undefined), 3);
  assert.equal(scan(2, 0), 0);
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
import { keyed, plain, reflective, reflectiveText, shelf, shelfText, viaString, wrapped } from '../src/source.js';

test('source read three ways keeps its text', () => {
  assert.deepEqual(Object.keys(keyed), ['function plain(x) {\\n  return x + 1;\\n}']);
  assert.equal(wrapped, '(function concatenated(x) {\\n  return x * 2;\\n})');
  assert.equal(viaString, 'function stringified(x) {\\n  return x - 1;\\n}');
  assert.equal(plain(1), 2);
  assert.ok(reflectiveText.startsWith('function reflective(o, x, y) {\\n  let v = x;'));
  assert.equal(reflective({ f: () => 'w' }, 1, 2), 'w');
  assert.equal(reflective(null, 0, 3), 0);
  assert.equal(shelfText, "label(name) { return name ? name.toUpperCase() : 'none'; }");
  assert.equal(shelf.label('a'), 'A');
});
`,
  'test/legacy.test.cjs': `const { strict: assert } = require('node:assert');
const test = require('node:test');
const { lookup, inside } = require('../src/legacy.cjs');

test('with and eval still read the scope', () => {
  assert.equal(lookup({ answer: 42 }, 'answer'), 42);
  assert.equal(inside({ kind: 'call', target: { go: () => 'went' }, left: 1, right: 2 }), 'went');
  assert.equal(inside({ kind: 'call', target: null, left: 1, right: 2 }), 2);
  assert.equal(inside({ kind: 'other', target: null, left: 0, right: 3 }), 3);
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
  'src/grouped.js': `export function both(a, b) {
  const ok = (a && b);
  return ok;
}

export function grouped(a, b, c) {
  return (a || b) && c;
}

export function inIf(a, b) {
  if ((a && b)) {
    return 1;
  }
  return 0;
}

export function call(box, name) {
  return box[name]?.() ?? 'none';
}

export function chain(o) {
  return o?.inner?.['value'] ?? -1;
}

export function nested(a, b) {
  const result = ((a)) ? ((b || a)) : null;
  return result;
}
`,
  'test/grouped.test.mjs': `import assert from 'node:assert/strict';
import test from 'node:test';
import { both, call, chain, grouped, inIf, nested } from '../src/grouped.js';

test('parenthesized logic, computed optional calls and chains', () => {
  assert.equal(both(1, 2), 2);
  assert.equal(both(0, 2), 0);
  assert.equal(both(1, 0), 0);
  assert.equal(grouped(1, 0, 1), 1);
  assert.equal(grouped(0, 1, 1), 1);
  assert.equal(grouped(0, 0, 1), 0);
  assert.equal(grouped(1, 0, 0), 0);
  assert.equal(inIf(1, 1), 1);
  assert.equal(inIf(0, 1), 0);
  assert.equal(inIf(1, 0), 0);
  assert.equal(call({ m: () => 1 }, 'm'), 1);
  assert.equal(call({}, 'm'), 'none');
  assert.equal(call({ m: () => null }, 'm'), 'none');
  assert.equal(chain({ inner: { value: 3 } }), 3);
  assert.equal(chain(null), -1);
  assert.equal(chain({}), -1);
  assert.equal(chain({ inner: {} }), -1);
  assert.equal(nested(1, 0), 1);
  assert.equal(nested(1, 2), 2);
  assert.equal(nested(0, 0), null);
});
`,
  'test/routes.test.mjs': `import assert from 'node:assert/strict';
import test from 'node:test';
import ItemsPage, { action, headers, helper, loader, page, title } from '../app/routes/items.ts';
import * as reexported from '../app/routes/reexport.ts';
import handleRequest, { streamTimeout } from '../app/entry.server.ts';

test('route handlers and the server entry', async () => {
  assert.equal(await loader({ request: new Request('http://x/items?q=a') }), 'a');
  assert.equal(await loader({ request: new Request('http://x/items') }), 'all');
  assert.equal(await action({ request: new Request('http://x/items', { method: 'POST' }) }), 'created');
  assert.equal(await action({ request: new Request('http://x/items', { method: 'PUT' }) }), 'ignored');
  assert.equal(title, 'Items');
  assert.equal(helper(), 'help');
  assert.deepEqual(headers(), { 'cache-control': 'no-store' });
  assert.equal(page, 'items');
  assert.equal(ItemsPage(), 'page');
  assert.equal(await reexported.loader(), 'shared');
  assert.equal(reexported.extra(), 'extra');
  assert.equal(reexported['extra-name'](), 'extra');
  assert.equal(reexported.unrelated(), 'extra');
  assert.equal(streamTimeout, 5000);
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
  for (const file of ['src/flow.js', 'src/values.js', 'src/grouped.js', 'src/typed.ts', 'src/cli.js', 'app/routes/items.ts', 'app/routes/shared.ts', 'app/entry.server.ts']) {
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
