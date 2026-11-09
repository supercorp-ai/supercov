// JavaScript self-time in the browser's pages while a Playwright suite runs:
// connects to the port the browser was launched with, records V8's sampling
// profiler as trace chunks of fifteen seconds -- a page that closes keeps its
// samples, which a profiler attached to the page itself would lose -- and
// writes what each function spent, by script, after every chunk. The browser
// may start late and start again; the profiler waits for it each time.
//
//   node renderer-profile.mjs <port> <output.json>
import { writeFileSync } from 'node:fs';

const [port, output] = process.argv.slice(2);
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const functions = new Map();
const profiles = new Map();
let sampledUs = 0;
let idleUs = 0;
let chunks = 0;

function save() {
  writeFileSync(
    output,
    JSON.stringify({
      chunks,
      busyMs: Math.round(sampledUs / 1000),
      idleMs: Math.round(idleUs / 1000),
      functions: [...functions].sort(([, a], [, b]) => b - a).slice(0, 500).map(([what, us]) => [what, Math.round(us / 1000)]),
    }),
  );
}

function place(callFrame) {
  const name = callFrame.functionName || '(anonymous)';
  if (!callFrame.url) return name;
  const script = callFrame.url.split('?')[0].split('/').slice(-2).join('/');
  return `${name} ${script}:${callFrame.lineNumber + 1}`;
}

function aggregate(trace) {
  const events = Array.isArray(trace) ? trace : trace.traceEvents ?? [];
  for (const event of events) {
    if (event.name !== 'ProfileChunk') continue;
    const key = `${event.pid}:${event.id}`;
    let nodes = profiles.get(key);
    if (!nodes) profiles.set(key, (nodes = new Map()));
    const profile = event.args?.data?.cpuProfile ?? {};
    for (const node of profile.nodes ?? []) nodes.set(node.id, place(node.callFrame));
    const deltas = event.args?.data?.timeDeltas ?? [];
    (profile.samples ?? []).forEach((sample, index) => {
      const what = nodes.get(sample) ?? '(unknown)';
      const us = Math.max(0, deltas[index] ?? 0);
      if (what === '(idle)') {
        idleUs += us;
        return;
      }
      sampledUs += us;
      functions.set(what, (functions.get(what) ?? 0) + us);
    });
  }
}

async function endpoint() {
  for (;;) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (response.ok) return (await response.json()).webSocketDebuggerUrl;
    } catch {}
    await sleep(250);
  }
}

async function record(url) {
  const socket = new WebSocket(url);
  await new Promise((opened, failed) => {
    socket.onopen = opened;
    socket.onerror = failed;
  });
  let closed = false;
  const ended = new Promise((done) => {
    socket.onclose = () => {
      closed = true;
      done();
    };
  });
  let next = 0;
  const pending = new Map();
  const listeners = new Set();
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    if (message.id !== undefined) {
      pending.get(message.id)?.(message);
      pending.delete(message.id);
    } else for (const listener of listeners) listener(message);
  };
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      if (closed) return reject(new Error('closed'));
      const id = ++next;
      pending.set(id, (message) => (message.error ? reject(new Error(message.error.message)) : resolve(message.result)));
      socket.send(JSON.stringify({ id, method, params }));
    });
  while (!closed) {
    await send('Tracing.start', {
      transferMode: 'ReturnAsStream',
      traceConfig: { includedCategories: ['disabled-by-default-v8.cpu_profiler'] },
    });
    await Promise.race([sleep(15_000), ended]);
    if (closed) return;
    const complete = new Promise((done) => {
      const listener = (message) => {
        if (message.method !== 'Tracing.tracingComplete') return;
        listeners.delete(listener);
        done(message.params);
      };
      listeners.add(listener);
    });
    await send('Tracing.end');
    const { stream } = await Promise.race([complete, ended.then(() => ({}))]);
    if (!stream) return;
    const parts = [];
    for (;;) {
      const read = await send('IO.read', { handle: stream, size: 4 << 20 });
      parts.push(read.base64Encoded ? Buffer.from(read.data, 'base64') : Buffer.from(read.data));
      if (read.eof) break;
    }
    await send('IO.close', { handle: stream }).catch(() => {});
    aggregate(JSON.parse(Buffer.concat(parts).toString()));
    chunks += 1;
    save();
  }
}

save();
for (;;) {
  try {
    await record(await endpoint());
  } catch (error) {
    console.log(`[renderer-profile] ${error.message}`);
  }
  await sleep(250);
}
