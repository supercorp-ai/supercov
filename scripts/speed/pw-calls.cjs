// DIAGNOSTIC ONLY (never merged): every Playwright protocol call a process
// makes, by type and method, whether internal, how many failed, and the
// synchronous CPU each send took on the calling thread. Loaded with
// NODE_OPTIONS=--require; written to $PW_CALLS_DIR/calls-<pid>.json at exit.
const { writeFileSync, mkdirSync } = require('node:fs');
const { join } = require('node:path');

const out = process.env.PW_CALLS_DIR;
const counts = {};
let patched = false;

function patch(connection) {
  const prototype = Object.getPrototypeOf(connection);
  if (!prototype || prototype.__pwCallsPatched) return;
  prototype.__pwCallsPatched = true;
  patched = true;
  const send = prototype.sendMessageToServer;
  prototype.sendMessageToServer = function (object, method, params, options) {
    const key = `${object?._type}.${method}${options?.internal ? ' (internal)' : ''}`;
    const entry = (counts[key] ??= { calls: 0, errors: 0, firstErrors: [] });
    entry.calls += 1;
    const pending = send.apply(this, arguments);
    pending.then(undefined, (error) => {
      entry.errors += 1;
      if (entry.firstErrors.length < 3) entry.firstErrors.push(String(error?.message ?? error).split('\n')[0].slice(0, 200));
    });
    return pending;
  };
  const dispatch = prototype.dispatch;
  prototype.dispatch = function (message) {
    if (!message.id && message.method) {
      const key = `event ${this._objects?.get(message.guid)?._type ?? '?'}.${message.method}`;
      (counts[key] ??= { calls: 0, errors: 0, firstErrors: [] }).calls += 1;
    }
    return dispatch.apply(this, arguments);
  };
}

function find() {
  if (patched) return;
  for (const [key, module] of Object.entries(require.cache)) {
    if (!/[/\\]playwright-core[/\\]index\.js$/.test(key)) continue;
    const connection = module.exports?._connection;
    if (connection && typeof connection.sendMessageToServer === 'function') patch(connection);
  }
}

if (out) {
  const timer = setInterval(find, 20);
  timer.unref();
  process.on('exit', () => {
    if (!patched) return;
    try {
      mkdirSync(out, { recursive: true });
      writeFileSync(join(out, `calls-${process.pid}.json`), JSON.stringify({ argv: process.argv.slice(1, 4), counts }, null, 1));
    } catch {}
  });
}
