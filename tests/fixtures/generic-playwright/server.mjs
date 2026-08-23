import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { extname, resolve } from "node:path";

const root = resolve("dist");
const port = 4397;
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };

const server = createServer(async (request, response) => {
  const pathname = new URL(request.url ?? "/", `http://localhost:${port}`).pathname;
  if (pathname === "/coverage-headers") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({
      scope: request.headers["x-supercov-scope"] ?? null,
      phase: request.headers["x-supercov-phase"] ?? null,
    }));
    return;
  }
  const path = resolve(root, pathname === "/" ? "index.html" : `.${pathname}`);
  try {
    const body = await readFile(path);
    response.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" });
    response.end(body);
  } catch {
    response.writeHead(404);
    response.end("not found");
  }
});

server.on("upgrade", (request, socket) => {
  const key = request.headers["sec-websocket-key"];
  if (typeof key !== "string") return socket.destroy();
  const accept = createHash("sha1")
    .update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`)
    .digest("base64");
  socket.write(
    `HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`,
  );
  const payload = Buffer.from(
    JSON.stringify({
      scope: request.headers["x-supercov-scope"] ?? null,
      phase: request.headers["x-supercov-phase"] ?? null,
    }),
  );
  const frameHeader =
    payload.length < 126
      ? Buffer.from([0x81, payload.length])
      : Buffer.from([0x81, 126, payload.length >> 8, payload.length & 0xff]);
  socket.write(Buffer.concat([frameHeader, payload]));
  socket.end();
});

server.listen(port);
