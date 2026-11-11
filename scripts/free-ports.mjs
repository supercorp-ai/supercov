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
import { createServer } from 'node:net';

const FIRST = 20000;
const COUNT = 12768;

const held = (port) =>
  new Promise((settle) => {
    const server = createServer();
    server.once('error', () => settle(null));
    server.listen(port, '127.0.0.1', () => settle(server));
  });

/** `count` distinct free ports: all are held at once, so none repeats. */
export async function freePorts(count) {
  const servers = [];
  for (let attempt = 0; servers.length < count; attempt += 1) {
    if (attempt === 1000) throw new Error(`no ${count} free port(s) in ${FIRST}-${FIRST + COUNT - 1}`);
    const server = await held(FIRST + Math.floor(Math.random() * COUNT));
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
