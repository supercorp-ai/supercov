// DIAGNOSTIC ONLY (never merged). Drives the generated large app's flow
// against two servers, plain and measured or two measured builds, one new
// context per flow, phases emulated before each action as the collector's
// would set them, and reports:
//   abba  <app> <urlA> <urlB> <rounds>   paired renderer metrics, ABBA order
//   trace <app> <urlA> <urlB> <flows>    per renderer thread CPU and main-thread
//                                         event self time from Chrome traces
//   opt   <app> <url> <flows>            V8 optimising compiles and deopts
import { createRequire } from 'node:module';
import { readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';

const [mode, app, ...rest] = process.argv.slice(2);
// The browser's own output (V8's --trace-opt) reaches Playwright's debug log only.
const browserLines = [];
if (mode === 'opt') {
  process.env.DEBUG = 'pw:browser';
  const write = process.stderr.write.bind(process.stderr);
  process.stderr.write = (chunk, ...args) => { const text = String(chunk); if (text.includes('pw:browser')) { browserLines.push(...text.split('\n')); return true; } return write(chunk, ...args); };
}
const { chromium } = createRequire(join(app, 'package.json'))('playwright');
const median = (xs) => { const s = [...xs].sort((a, b) => a - b); return s[Math.floor(s.length / 2)]; };
const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
let phaseCounter = 0;
async function phase(page) {
  phaseCounter += 1;
  const id = `phase-${phaseCounter}`;
  await page.evaluate((id) => { globalThis.__SUPERCOV_PHASE_ID__ = id; globalThis.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__?.(globalThis.__SUPERCOV_MCDC_TEST_ID__ ?? 'unscoped', id); }, id).catch(() => {});
}
async function flow(page, base, section) {
  const others = [1, 2, 3].map((step) => (section + step * 5) % 24);
  await page.goto(`${base}/s/${section}`);
  await page.getByRole('heading', { name: `Section ${section}` }).waitFor();
  await phase(page);
  await page.getByRole('link', { name: `Item ${section}-1`, exact: true }).click();
  await page.getByRole('heading', { name: `Item ${section}-1` }).waitFor();
  await phase(page);
  await page.goBack();
  await page.getByLabel('Title').waitFor();
  await phase(page);
  await page.getByLabel('Title').fill(`Added p${section}`);
  await phase(page);
  await page.getByRole('button', { name: 'Add' }).click();
  for (const other of others) {
    await phase(page);
    await page.getByRole('link', { name: `Section ${other}`, exact: true }).click();
    await page.getByRole('heading', { name: `Section ${other}` }).waitFor();
  }
}

if (mode === 'abba') {
  const [aUrl, bUrl, rounds] = rest;
  const browser = await chromium.launch();
  const results = { A: [], B: [] }; const pairs = [];
  async function once(variant, section) {
    const context = await browser.newContext(); const page = await context.newPage();
    const cdp = await context.newCDPSession(page); await cdp.send('Performance.enable');
    const started = performance.now();
    await flow(page, variant === 'A' ? aUrl : bUrl, section);
    const wall = performance.now() - started;
    const { metrics } = await cdp.send('Performance.getMetrics');
    const m = Object.fromEntries(metrics.map((x) => [x.name, x.value]));
    results[variant].push({ wall, script: m.ScriptDuration * 1000, task: m.TaskDuration * 1000 });
    await context.close();
  }
  for (let i = 0; i < 2; i += 1) { await once('A', i); await once('B', i); }
  results.A = []; results.B = [];
  for (let round = 0; round < Number(rounds); round += 1) {
    for (const v of round % 2 === 0 ? ['A', 'B', 'B', 'A'] : ['B', 'A', 'A', 'B']) await once(v, round % 24);
    const last = (v) => results[v].slice(-2);
    pairs.push(Object.fromEntries(['wall', 'script', 'task'].map((k) => [k, (last('B')[0][k] + last('B')[1][k] - last('A')[0][k] - last('A')[1][k]) / 2])));
  }
  await browser.close();
  const out = {};
  for (const k of ['wall', 'script', 'task']) {
    const xs = pairs.map((p) => p[k]); const m = mean(xs);
    const se = Math.sqrt(xs.reduce((a, x) => a + (x - m) ** 2, 0) / (xs.length - 1) / xs.length);
    out[k] = { A: +median(results.A.map((r) => r[k])).toFixed(1), B: +median(results.B.map((r) => r[k])).toFixed(1), pairedDiff: +m.toFixed(2), se: +se.toFixed(2) };
  }
  console.log(JSON.stringify({ mode, aUrl, bUrl, flows: results.A.length, ...out }));
}

if (mode === 'trace') {
  const [aUrl, bUrl, flows] = rest;
  const browser = await chromium.launch();
  const categories = ['devtools.timeline', 'v8', 'v8.execute', 'disabled-by-default-v8.compile', 'disabled-by-default-devtools.timeline', 'blink', 'loading', 'disabled-by-default-v8.gc'];
  const threads = { A: {}, B: {} }; const events = { A: {}, B: {} };
  const n = Number(flows);
  for (let i = 0; i < 2 * n + 2; i += 1) {
    const v = [0, 3].includes(i % 4) ? 'A' : 'B';
    const context = await browser.newContext(); const page = await context.newPage();
    const path = `/tmp/probe-diag-trace-${i}.json`;
    await browser.startTracing(page, { path, categories });
    await flow(page, v === 'A' ? aUrl : bUrl, (i >> 1) % 24);
    await browser.stopTracing(); await context.close();
    if (i < 2) { rmSync(path); continue; }
    const ev = JSON.parse(readFileSync(path, 'utf8')).traceEvents; rmSync(path);
    const procs = new Map(ev.filter((e) => e.name === 'process_name').map((e) => [e.pid, e.args.name]));
    const names = new Map(ev.filter((e) => e.name === 'thread_name').map((e) => [`${e.pid}:${e.tid}`, e.args.name]));
    const by = new Map();
    for (const e of ev) { if (e.ph !== 'X' || !e.dur) continue; const k = `${e.pid}:${e.tid}`; (by.get(k) ?? by.set(k, []).get(k)).push(e); }
    for (const [k, evs] of by) {
      evs.sort((a, b) => a.ts - b.ts || b.dur - a.dur);
      const key = `${procs.get(+k.split(':')[0]) ?? '?'} | ${(names.get(k) ?? '?').replace(/\d+$/, '')}`;
      let end = -1;
      for (const e of evs) { if (e.ts >= end) { threads[v][key] = (threads[v][key] ?? 0) + (e.tdur ?? e.dur) / 1000 / n; end = e.ts + e.dur; } }
      if (names.get(k) !== 'CrRendererMain') continue;
      const open = [];
      for (const e of evs) {
        while (open.length && open.at(-1).ts + open.at(-1).dur <= e.ts) open.pop();
        if (open.length) open.at(-1).self -= Math.min(e.dur, open.at(-1).ts + open.at(-1).dur - e.ts);
        e.self = e.dur; open.push(e);
      }
      for (const e of evs) events[v][e.name] = (events[v][e.name] ?? 0) + e.self / 1000 / n;
    }
  }
  await browser.close();
  for (const [label, table, limit] of [['thread CPU ms per flow', threads, 12], ['main-thread self ms per flow', events, 25]]) {
    console.log(`== ${label}: plain-or-A, B, B-A`);
    const keys = new Set([...Object.keys(table.A), ...Object.keys(table.B)]);
    const rows = [...keys].map((k) => [k, table.A[k] ?? 0, table.B[k] ?? 0]).sort((x, y) => Math.abs(y[2] - y[1]) - Math.abs(x[2] - x[1]));
    for (const [k, a, b] of rows.slice(0, limit)) console.log(`${(b - a).toFixed(2).padStart(8)} ${a.toFixed(2).padStart(8)} ${b.toFixed(2).padStart(8)}  ${k}`);
  }
}

if (mode === 'profile' || mode === 'ablate') {
  // profile: CPU profiles of both servers' pages, self time by script and function.
  // ablate: one server, B's runtime chunk with every probe helper made a no-op
  // (call sites kept), A's served unchanged through the same interception.
  const [aUrl, bUrl, flows] = rest;
  const browser = await chromium.launch();
  const agg = { A: { url: {}, fn: {} }, B: { url: {}, fn: {} } };
  const results = { A: [], B: [] };
  const noop = [
    ['i.hit=e=>{s[e]!==l.e&&tO(i,e)}', 'i.hit=e=>{}'],
    ['mcdcEndV2:function(e,t,r,n){if(0===r)return n;', 'mcdcEndV2:function(e,t,r,n){return n;'],
    ['function tR(e,t){let r=e.clock;', 'function tR(e,t){return;let r=e.clock;'],
    ['selectEnd2V2:function(e,t,r,n){return tR(e,t+2*!!n+ +!!r),r}', 'selectEnd2V2:function(e,t,r,n){return r}'],
  ];
  let patched = 0;
  const n = Number(flows);
  for (let i = 0; i < 2 * n + 2; i += 1) {
    const v = [0, 3].includes(i % 4) ? 'A' : 'B';
    const context = await browser.newContext(); const page = await context.newPage();
    if (mode === 'ablate') {
      await context.route('**/_next/static/chunks/*.js', async (route) => {
        const response = await route.fetch().catch(() => undefined);
        if (!response) return route.abort().catch(() => {});
        let body = await response.text();
        if (v === 'B' && body.includes('__SUPERCOV_ACTIVATE_PROBE_CONTEXT__')) for (const [from, to] of noop) if (body.includes(from)) { body = body.replace(from, to); patched += 1; }
        await route.fulfill({ response, body }).catch(() => {});
      });
    }
    const cdp = await context.newCDPSession(page);
    await cdp.send('Performance.enable');
    if (mode === 'profile') { await cdp.send('Profiler.enable'); await cdp.send('Profiler.setSamplingInterval', { interval: 200 }); await cdp.send('Profiler.start'); }
    await flow(page, mode === 'ablate' || v === 'A' ? aUrl : bUrl, (i >> 1) % 24);
    const { metrics } = await cdp.send('Performance.getMetrics');
    const m = Object.fromEntries(metrics.map((x) => [x.name, x.value]));
    if (i >= 2) results[v].push({ script: m.ScriptDuration * 1000, task: m.TaskDuration * 1000 });
    if (mode === 'ablate') await context.unrouteAll({ behavior: 'ignoreErrors' });
    if (mode === 'profile') {
      const { profile } = await cdp.send('Profiler.stop');
      if (i >= 2) {
        const nodes = new Map(profile.nodes.map((x) => [x.id, x]));
        for (let k = 0; k < profile.samples.length; k += 1) {
          const f = nodes.get(profile.samples[k]).callFrame; const dt = (profile.timeDeltas[k] ?? 0) / 1000 / n;
          if (f.functionName === '(idle)') continue;
          const url = f.url ? f.url.split('/').pop().slice(0, 40) : `(${f.functionName.startsWith('(') ? f.functionName : 'native ' + f.functionName})`;
          agg[v].url[url] = (agg[v].url[url] ?? 0) + dt;
          const fk = `${f.functionName || '(anon)'} ${url}`; agg[v].fn[fk] = (agg[v].fn[fk] ?? 0) + dt;
        }
      }
    }
    await context.close();
  }
  await browser.close();
  const se = (xs) => { const m = mean(xs); return Math.sqrt(xs.reduce((a, x) => a + (x - m) ** 2, 0) / (xs.length - 1) / xs.length); };
  const summary = Object.fromEntries(['script', 'task'].map((k) => [k, { A: +mean(results.A.map((r) => r[k])).toFixed(1), B: +mean(results.B.map((r) => r[k])).toFixed(1), seA: +se(results.A.map((r) => r[k])).toFixed(1), seB: +se(results.B.map((r) => r[k])).toFixed(1) }]));
  console.log(JSON.stringify({ mode, flows: n, patchedChunks: patched, ...summary }));
  if (mode === 'profile') {
    for (const kind of ['url', 'fn']) {
      const keys = new Set([...Object.keys(agg.A[kind]), ...Object.keys(agg.B[kind])]);
      const rows = [...keys].map((k) => [k, agg.A[kind][k] ?? 0, agg.B[kind][k] ?? 0]).sort((x, y) => Math.max(y[1], y[2]) - Math.max(x[1], x[2]));
      console.log(`== sampled self ms per flow by ${kind}: A, B (largest first)`);
      for (const [k, a, b] of rows.slice(0, kind === 'url' ? 25 : 60)) console.log(`${a.toFixed(2).padStart(8)} ${b.toFixed(2).padStart(8)}  ${k}`);
      const tot = (o) => Object.values(o).reduce((x, y) => x + y, 0);
      console.log(`total ${tot(agg.A[kind]).toFixed(1)} ${tot(agg.B[kind]).toFixed(1)}`);
    }
  }
}

if (mode === 'opt') {
  const [url, flows] = rest;
  const browser = await chromium.launch({ args: ['--js-flags=--trace-opt --trace-deopt'] });
  const lines = browserLines;
  for (let i = 0; i < Number(flows); i += 1) { const context = await browser.newContext(); const page = await context.newPage(); await flow(page, url, i % 24); await context.close(); }
  await browser.close();
  const agg = {}; let deopts = 0;
  for (const line of lines) {
    if (line.includes('bailout (kind')) deopts += 1;
    const m = /completed compiling .*\(target (\w+)\)( OSR)? - took ([\d.]+), ([\d.]+), ([\d.]+) ms/.exec(line);
    if (!m) continue;
    const k = m[1] + (m[2] ?? ''); agg[k] ??= [0, 0]; agg[k][0] += 1; agg[k][1] += +m[3] + +m[4] + +m[5];
  }
  console.log(JSON.stringify({ mode, url, flows: Number(flows), deopts, compiles: Object.fromEntries(Object.entries(agg).map(([k, [c, t]]) => [k, { count: c, ms: +t.toFixed(1) }])) }));
}
