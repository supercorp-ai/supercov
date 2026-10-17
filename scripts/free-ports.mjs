// Ports the system says are free, for gates that start servers. A random port
// in a fixed range met a process another gate left listening on a CI runner
// (EADDRINUSE); asking the system for each one does not.
import { createServer } from 'node:net';

/** `count` distinct free ports: all are held at once, so none repeats. */
export async function freePorts(count) {
  const servers = await Promise.all(
    Array.from(
      { length: count },
      () =>
        new Promise((settle, fail) => {
          const server = createServer();
          server.once('error', fail);
          server.listen(0, '127.0.0.1', () => settle(server));
        }),
    ),
  );
  const ports = servers.map((server) => server.address().port);
  await Promise.all(servers.map((server) => new Promise((done) => server.close(done))));
  return ports;
}
