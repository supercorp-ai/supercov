// A local server that answers the way Jev does, for gates that follow a
// project through `assertions assess`, `quality` and `security` with no key
// and no network. It judges nothing: `ANSWER=yes` says yes to every yes/no
// question and picks the first option other than `none` for every choice,
// `ANSWER=no` the opposite.
//
// Prints its port on the first line of stdout. After each request it writes
// how many it has answered to the file named by `SEEN`.
import http from 'node:http';
import { writeFileSync } from 'node:fs';

const yes = process.env.ANSWER !== 'no';
const seen = process.env.SEEN;
let requests = 0;

function answer(question) {
  if (question.type !== 'choice') return { type: 'noul', noul: yes ? 0.9 : 0.1 };
  const options = Object.keys(question.criteria ?? {});
  const chosen = (yes ? options.find((option) => option !== 'none') : options.find((option) => option === 'none')) ?? options[0];
  return {
    type: 'choice',
    choice: chosen,
    probabilities: Object.fromEntries(options.map((option) => [option, option === chosen ? 1 : 0])),
    confidence: 1,
  };
}

const server = http.createServer((request, response) => {
  const chunks = [];
  request.on('data', (chunk) => chunks.push(chunk));
  request.on('end', () => {
    const body = Buffer.concat(chunks);
    const asked = JSON.parse(body.toString());
    requests += 1;
    if (seen) writeFileSync(seen, String(requests));
    const reply = JSON.stringify({
      model: 'typesafe/jev-1.13-20260917',
      answers: Object.fromEntries(Object.entries(asked.questions ?? {}).map(([id, question]) => [id, answer(question)])),
      usage: { input_tokens: Math.ceil(body.length / 3), output_tokens: 0 },
    });
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(reply);
  });
});
server.listen(0, '127.0.0.1', () => console.log(server.address().port));
