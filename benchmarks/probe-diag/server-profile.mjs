// DIAGNOSTIC ONLY. A server's .cpuprofile files, self time by kind of script and by function.
import { readFileSync, readdirSync } from 'node:fs';
const dir = process.argv[2];
const byKind = {}; const byFn = {}; let total = 0;
const kind = (url) => {
  if (!url) return '(native/vm)';
  if (url.includes('.supercov/node_modules/runtime')) return 'supercov runtime';
  if (url.includes('.supercov/')) return 'supercov other';
  if (url.includes('/.next/server/chunks/ssr/') || url.includes('/.next/server/app/')) return 'app server chunks';
  if (url.includes('/node_modules/next/')) return 'next';
  if (url.includes('/node_modules/react')) return 'react';
  if (url.startsWith('node:')) return 'node internals';
  return 'other ' + url.split('/').slice(-2).join('/');
};
for (const file of readdirSync(dir).filter((f) => f.endsWith('.cpuprofile'))) {
  const p = JSON.parse(readFileSync(`${dir}/${file}`, 'utf8'));
  const nodes = new Map(p.nodes.map((n) => [n.id, n]));
  for (let i = 0; i < p.samples.length; i += 1) {
    const f = nodes.get(p.samples[i]).callFrame; const dt = (p.timeDeltas[i] ?? 0) / 1000;
    if (f.functionName === '(idle)' || f.functionName === '(program)' && false) continue;
    total += dt; const k = f.functionName.startsWith('(') ? f.functionName : kind(f.url);
    byKind[k] = (byKind[k] ?? 0) + dt;
    const fk = `${f.functionName || '(anon)'} ${f.url.includes('.supercov/') ? '.supercov/' : ''}${f.url.split('/').slice(-1)[0]}`; byFn[fk] = (byFn[fk] ?? 0) + dt;
  }
}
console.log(`sampled ms ${total.toFixed(0)}`);
for (const [k, v] of Object.entries(byKind).sort((a, b) => b[1] - a[1])) console.log(`${v.toFixed(0).padStart(8)}  ${k}`);
for (const [k, v] of Object.entries(byFn).sort((a, b) => b[1] - a[1]).slice(0, 25)) console.log(`${v.toFixed(0).padStart(8)}  ${k}`);
console.log('-- Supercov functions');
for (const [k, v] of Object.entries(byFn).filter(([k]) => /runtime\.mjs|bootstrap|launchSupervisor|transport|snapshots|resolve-loader|register|capability|provenance|\.supercov/.test(k)).sort((a, b) => b[1] - a[1]).slice(0, 30)) console.log(`${v.toFixed(0).padStart(8)}  ${k}`);
