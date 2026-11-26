// Ports free right now, for gates that start servers on a port they name.
//
// Drawn at random from 20000-32767, below every system's ephemeral range
// (Linux's starts at 32768, macOS's and Windows' at 49152), and each one
// bound before it is given. A port the system picked (listening on port 0)
// was handed out again to whatever asked next in the gap before the gate's
// server bound it: wrangler's own servers, Chrome's driver, an outgoing
// connection; the gate then failed with EADDRINUSE or a server that never
// answered. Nothing is handed a port from below that range unless it names
// one, and one a leftover server holds fails its bind and is drawn again.
//
// And each one claimed until this process exits: a file of its number in
// the temporary directory's `supercov-ports`, holding this process's id, as
// the Rust tests claim theirs (`free_port`, tests/common/mod.rs). A port
// another gate or test claimed is not drawn, though no server holds it yet;
// a claim whose process is gone is stale, and freed.
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const FIRST = 20000;
const COUNT = 12768;

const held = (port) =>
  new Promise((settle) => {
    const server = createServer();
    server.once('error', () => settle(null));
    server.listen(port, '127.0.0.1', () => settle(server));
  });

const claims = join(tmpdir(), 'supercov-ports');
const claimed = [];
process.once('exit', () => {
  for (const claim of claimed) rmSync(claim, { force: true });
});

/** Claims `port` for this process: false where another live one holds it. */
function claim(port) {
  const path = join(claims, String(port));
  try {
    writeFileSync(path, String(process.pid), { flag: 'wx' });
  } catch {
    // Empty while its holder is still writing it: held.
    let holder = 0;
    try {
      holder = Number(readFileSync(path, 'utf8') || 0);
    } catch {
      // Released since.
    }
    if (holder > 0) {
      try {
        process.kill(holder, 0);
      } catch (error) {
        if (error.code === 'ESRCH') rmSync(path, { force: true });
      }
    }
    return false;
  }
  claimed.push(path);
  return true;
}

/** `count` distinct free ports: all are held at once, so none repeats. */
export async function freePorts(count) {
  mkdirSync(claims, { recursive: true });
  const servers = [];
  for (let attempt = 0; servers.length < count; attempt += 1) {
    if (attempt === 1000) throw new Error(`no ${count} free port(s) in ${FIRST}-${FIRST + COUNT - 1}`);
    const port = FIRST + Math.floor(Math.random() * COUNT);
    if (!claim(port)) continue;
    const server = await held(port);
    if (server) servers.push(server);
  }
  const ports = servers.map((server) => server.address().port);
  await Promise.all(servers.map((server) => new Promise((done) => server.close(done))));
  return ports;
}

/** One free port. */
export async function freePort() {
  return (await freePorts(1))[0];
}
