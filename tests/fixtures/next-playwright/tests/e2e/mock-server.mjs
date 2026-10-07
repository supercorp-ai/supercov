// A second server beside the application, as a suite with mocked services has.
import { createServer } from 'node:http';

createServer((_, response) => {
  response.setHeader('content-type', 'application/json');
  response.end(JSON.stringify({ mock: true }));
}).listen(Number(process.argv[2]), '127.0.0.1');
