// DIAGNOSTIC ONLY: a browser suite that runs the same way every time, for
// proving that a collector change measures nothing differently: static pages
// whose handlers do all their work inside the event the test dispatches, an
// iframe, a second page of the context, a context the test opens itself, and
// navigations back and forth.
//
//   node scripts/speed/exact-bench.mjs <directory> <port>
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

const [directory, port = '4471'] = process.argv.slice(2);
const MODULES = 12;
const TESTS = 24;
const files = {};
const write = (path, text) => { files[path] = text; };

write('package.json', '{ "name": "exact-bench", "private": true, "type": "module", "devDependencies": { "@playwright/test": "1.60.0" } }\n');
write('server.mjs', `import { createServer } from 'node:http';
import { readFileSync } from 'node:fs';

createServer((request, response) => {
  const path = new URL(request.url, 'http://x').pathname;
  const file = path === '/' ? 'index.html' : path.slice(1);
  try {
    const body = readFileSync(new URL(\`./public/\${file}\`, import.meta.url));
    response.writeHead(200, { 'content-type': file.endsWith('.js') ? 'text/javascript' : 'text/html' });
    response.end(body);
  } catch {
    response.writeHead(404);
    response.end();
  }
}).listen(${port});
`);
for (let m = 0; m < MODULES; m += 1) {
  write(`public/lib/m${m}.js`, `export function grade${m}(value, flag) {
  let total = 0;
  for (let i = 0; i < (value % 7) + 1; i += 1) {
    if (i % 2 === 0 && flag) total += i * ${m + 1};
    else if (i % 3 === 0 || value > ${m * 3}) total -= 1;
    else total += 2;
  }
  const label = total > ${m * 2} ? 'high' : total < 0 ? 'low' : 'mid';
  return flag && value > 10 ? \`\${label}+\` : label;
}

export function format${m}(text) {
  if (!text) return 'none';
  const words = text.split(' ').filter((word) => word.length > ${m % 4});
  return words.length > 2 ? words.slice(0, 2).join('-') : words.join('_') || 'short';
}
`);
}
write('public/main.js', `${Array.from({ length: MODULES }, (_, m) => `import { grade${m}, format${m} } from './lib/m${m}.js';`).join('\n')}

const grades = [${Array.from({ length: MODULES }, (_, m) => `grade${m}`).join(', ')}];
const formats = [${Array.from({ length: MODULES }, (_, m) => `format${m}`).join(', ')}];
let presses = 0;
const out = document.getElementById('out');
const input = document.getElementById('text');
for (let m = 0; m < grades.length; m += 1) {
  const button = document.createElement('button');
  button.id = \`b\${m}\`;
  button.textContent = \`run \${m}\`;
  button.addEventListener('click', () => {
    presses += 1;
    out.textContent = \`\${grades[m](presses + m, presses % 2 === 0)} \${formats[m](input.value)} \${presses}\`;
  });
  document.body.append(button);
}
out.textContent = 'ready';
`);
write('public/index.html', '<!doctype html><input id="text"><p id="out">loading</p><iframe id="frame" src="/frame.html"></iframe><script type="module" src="/main.js"></script>\n');
write('public/frame.js', `import { grade1 } from './lib/m1.js';
const here = document.getElementById('here');
document.getElementById('go').addEventListener('click', () => { here.textContent = grade1(5, true); });
here.textContent = 'framed';
`);
write('public/frame.html', '<!doctype html><button id="go">go</button><p id="here">loading</p><script type="module" src="/frame.js"></script>\n');
write('public/second.js', `import { format3 } from './lib/m3.js';
import { grade2 } from './lib/m2.js';
const out = document.getElementById('second');
document.getElementById('again').addEventListener('click', () => { out.textContent = \`\${format3('a b c d')} \${grade2(12, false)}\`; });
out.textContent = 'second ready';
`);
write('public/second.html', '<!doctype html><button id="again">again</button><p id="second">loading</p><script type="module" src="/second.js"></script>\n');
write('playwright.config.js', `export default {
  testDir: 'tests',
  workers: 1,
  retries: 0,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}', browserName: 'chromium' },
  webServer: { command: 'node server.mjs', url: 'http://127.0.0.1:${port}/', reuseExistingServer: false },
};
`);
const tests = [];
for (let t = 0; t < TESTS; t += 1) {
  const a = t % MODULES, b = (t * 5 + 1) % MODULES, c = (t * 7 + 2) % MODULES;
  if (t % 6 === 5) {
    tests.push(`test('own context ${t}', async ({ browser }) => {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto('/');
  await expect(page.locator('#out')).toHaveText('ready');
  await page.locator('#b${a}').click();
  await expect(page.locator('#out')).toContainText(' 1');
  await context.close();
});`);
    continue;
  }
  tests.push(`test('flow ${t}', async ({ page, context }) => {
  await page.goto('/');
  await expect(page.locator('#out')).toHaveText('ready');
  await page.locator('#text').fill('alpha beta gamma ${t}');
  await page.locator('#b${a}').click();
  await expect(page.locator('#out')).toContainText(' 1');
  await page.locator('#b${b}').click();
  await page.locator('#b${c}').click();
  await expect(page.locator('#out')).toContainText(' 3');
  await page.frameLocator('#frame').locator('#go').click();
  await expect(page.frameLocator('#frame').locator('#here')).not.toHaveText('framed');
  await page.goto('/second.html');
  await page.locator('#again').click();
  await expect(page.locator('#second')).not.toHaveText('second ready');
  await page.goBack();
  await expect(page.locator('#out')).toHaveText('ready');
${t % 4 === 1 ? `  const other = await context.newPage();
  await other.goto('/second.html');
  await other.locator('#again').click();
  await expect(other.locator('#second')).not.toHaveText('second ready');
  await other.close();
` : ''}  expect(${t} % 2).toBe(${t % 2});
});`);
}
write('tests/bench.spec.js', `import { test, expect } from '@playwright/test';\n\n${tests.join('\n\n')}\n`);
for (const [path, text] of Object.entries(files)) {
  mkdirSync(dirname(join(directory, path)), { recursive: true });
  writeFileSync(join(directory, path), text);
}
console.log(`[exact-bench] ${Object.keys(files).length} files in ${directory}: ${TESTS} tests`);
