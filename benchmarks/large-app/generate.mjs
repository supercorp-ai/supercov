// A large Next.js app and its Playwright suite, generated: what a measured
// browser suite costs where pages are heavy and every test loads many.
//
//   node benchmarks/large-app/generate.mjs <directory> [--sections 24] [--components 240] [--tests 300]
//
// Each section has a server-rendered list page and a detail page reading
// node:sqlite, an API route that adds to the list, and a board of client
// components with state, conditions and derived values, so its chunk is
// heavy. Each test opens a section, follows a client-side link, adds an item
// through the API and sees it listed, then visits three other sections: six
// page loads or navigations, in a context of its own. The same arguments give
// the same files byte for byte.
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

const argv = process.argv.slice(2);
const directory = argv[0];
if (!directory) throw new Error('usage: generate.mjs <directory> [--sections N] [--components N] [--tests N]');
const option = (name, fallback) => {
  const at = argv.indexOf(`--${name}`);
  return at === -1 ? fallback : Number(argv[at + 1]);
};
const SECTIONS = option('sections', 24);
const COMPONENTS = option('components', 240);
const TESTS = option('tests', 300);
const PER_FILE = 10;

const files = {};
const write = (path, text) => { files[path] = text; };

write('package.json', `${JSON.stringify({
  name: 'large-app', private: true, version: '0.1.0',
  scripts: { build: 'next build', start: 'next start' },
  dependencies: { next: '16.1.1', react: '19.2.0', 'react-dom': '19.2.0' },
  devDependencies: { '@playwright/test': '1.60.0', '@types/node': '24.10.1', '@types/react': '19.2.2', typescript: '5.9.3' },
}, null, 2)}\n`);
write('next.config.ts', "import type { NextConfig } from 'next';\n\nconst config: NextConfig = {};\nexport default config;\n");
write('tsconfig.json', `${JSON.stringify({
  compilerOptions: {
    target: 'ES2022', lib: ['dom', 'dom.iterable', 'esnext'], allowJs: false, skipLibCheck: true, strict: true, noEmit: true,
    esModuleInterop: true, module: 'esnext', moduleResolution: 'bundler', resolveJsonModule: true, isolatedModules: true,
    jsx: 'react-jsx', incremental: true, plugins: [{ name: 'next' }], paths: { '@/*': ['./*'] },
  },
  include: ['next-env.d.ts', '**/*.ts', '**/*.tsx', '.next/types/**/*.ts'],
  exclude: ['node_modules', 'tests'],
}, null, 2)}\n`);
write('.gitignore', 'node_modules\n.next\ntest-results\nplaywright-report\n');

// The database: one table of items per section, seeded on first use.
write('lib/db.ts', `import { DatabaseSync } from 'node:sqlite';

type Item = { id: number; section: number; title: string; score: number; done: number };
let database: DatabaseSync | undefined;

function open(): DatabaseSync {
  if (database) return database;
  database = new DatabaseSync(':memory:');
  database.exec('create table items (id integer primary key, section integer, title text, score integer, done integer)');
  const insert = database.prepare('insert into items (section, title, score, done) values (?, ?, ?, ?)');
  for (let section = 0; section < ${SECTIONS}; section += 1)
    for (let index = 0; index < 12; index += 1)
      insert.run(section, \`Item \${section}-\${index}\`, (section * 7 + index * 13) % 100, index % 3 === 0 ? 1 : 0);
  return database;
}

// node:sqlite gives rows with no prototype, which React will not hand to a
// client component: each is copied into a plain object.
const plain = (row: unknown) => ({ ...(row as Item) });

export function listItems(section: number): Item[] {
  return open().prepare('select * from items where section = ? order by id').all(section).map(plain);
}

export function getItem(id: number): Item | undefined {
  const row = open().prepare('select * from items where id = ?').get(id);
  return row === undefined ? undefined : plain(row);
}

export function addItem(section: number, title: string): Item {
  const score = title.length * 11 % 100;
  const result = open().prepare('insert into items (section, title, score, done) values (?, ?, ?, 0)').run(section, title, score);
  return getItem(Number(result.lastInsertRowid)) as Item;
}
`);

// Client components: state, conditions and derived values, each a little
// different, as a real app's are.
for (let index = 0; index < COMPONENTS; index += 1) {
  const kind = index % 4;
  write(`components/c${index}.tsx`, `'use client';
import { useMemo, useState } from 'react';

type Props = { items: { id: number; title: string; score: number; done: number }[]; seed: number };

export default function C${index}({ items, seed }: Props) {
  const [filter, setFilter] = useState(${kind === 0 ? "'all'" : kind === 1 ? "'open'" : "'all'"});
  const [limit, setLimit] = useState(${5 + (index % 7)});
  const visible = useMemo(() => {
    const kept = [];
    for (const item of items) {
      if (filter === 'open' && item.done) continue;
      if (filter === 'done' && !item.done) continue;
      if (item.score < ${index % 20} && seed % 2 === 0) continue;
      kept.push({ ...item, weight: item.score * ${(index % 5) + 1} + (item.title.length > 8 ? 3 : 1) });
    }
    return kept.sort((a, b) => (b.weight - a.weight) || (a.id - b.id)).slice(0, limit);
  }, [items, filter, limit, seed]);
  const total = visible.reduce((sum, item) => sum + (item.done ? 0 : item.weight), 0);
  const label = total > ${100 + index} ? 'busy' : total > ${20 + (index % 30)} ? 'steady' : 'quiet';
  return (
    <section data-component="c${index}" className={label}>
      <h3>Panel ${index} {label}</h3>
      <div>
        <button type="button" onClick={() => setFilter(filter === 'all' ? 'open' : filter === 'open' ? 'done' : 'all')}>{filter}</button>
        <button type="button" onClick={() => setLimit(limit > 3 ? limit - 1 : 10)}>{limit}</button>
      </div>
      <ul>
        {visible.map((item) => (
          <li key={item.id} className={item.done ? 'done' : item.weight > 50 ? 'heavy' : undefined}>
            {item.title} {item.done ? '(done)' : item.weight > ${40 + (index % 50)} ? '(high)' : null}
          </li>
        ))}
      </ul>
    </section>
  );
}
`);
}

const perSection = Math.max(1, Math.floor(COMPONENTS / SECTIONS));
write('app/layout.tsx', `import type { ReactNode } from 'react';
import Link from 'next/link';

export default function Layout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <body>
        <nav>{Array.from({ length: ${SECTIONS} }, (_, section) => <Link key={section} href={\`/s/\${section}\`}>Section {section}</Link>)}</nav>
        <main>{children}</main>
      </body>
    </html>
  );
}
`);
write('app/page.tsx', "export default function Home() {\n  return <h1>Large app</h1>;\n}\n");
for (let section = 0; section < SECTIONS; section += 1) {
  const imports = Array.from({ length: perSection }, (_, offset) => (section * perSection + offset) % COMPONENTS);
  write(`app/s/${section}/board.tsx`, `'use client';
import { useState } from 'react';
${imports.map((component) => `import C${component} from '@/components/c${component}';`).join('\n')}

type Item = { id: number; title: string; score: number; done: number };

export default function Board({ initial }: { initial: Item[] }) {
  const [items, setItems] = useState(initial);
  const [title, setTitle] = useState('');
  async function add() {
    const response = await fetch('/api/s/${section}', { method: 'POST', body: JSON.stringify({ title }), headers: { 'content-type': 'application/json' } });
    const added = (await response.json()) as Item;
    setItems([...items, added]);
    setTitle('');
  }
  return (
    <div>
      <input aria-label="Title" value={title} onChange={(event) => setTitle(event.target.value)} />
      <button type="button" onClick={add} disabled={title.length === 0}>Add</button>
      <p data-testid="count">{items.length} items</p>
${imports.map((component, index) => `      <C${component} items={items} seed={${index}} />`).join('\n')}
    </div>
  );
}
`);
  write(`app/s/${section}/page.tsx`, `import Link from 'next/link';
import { listItems } from '@/lib/db';
import Board from './board';

export const dynamic = 'force-dynamic';

export default function Page() {
  const items = listItems(${section});
  return (
    <div>
      <h1>Section ${section}</h1>
      <ul>{items.slice(0, 5).map((item) => <li key={item.id}><Link href={\`/s/${section}/\${item.id}\`}>{item.title}</Link></li>)}</ul>
      <Board initial={items} />
    </div>
  );
}
`);
  write(`app/s/${section}/[id]/page.tsx`, `import { notFound } from 'next/navigation';
import { getItem } from '@/lib/db';

export const dynamic = 'force-dynamic';

export default async function Detail({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const item = getItem(Number(id));
  if (!item || item.section !== ${section}) notFound();
  return <article><h1>{item.title}</h1><p>Score {item.score >= 50 ? 'high' : 'low'}</p></article>;
}
`);
  write(`app/api/s/${section}/route.ts`, `import { addItem, listItems } from '@/lib/db';

export async function GET() {
  return Response.json(listItems(${section}));
}

export async function POST(request: Request) {
  const body = (await request.json()) as { title?: string };
  if (!body.title || body.title.length > 80) return Response.json({ error: 'title' }, { status: 400 });
  return Response.json(addItem(${section}, body.title));
}
`);
}

write('tests/playwright.config.ts', `import { defineConfig } from '@playwright/test';

const port = Number(process.env.LARGE_APP_PORT ?? 3210);
export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.ts',
  workers: Number(process.env.LARGE_APP_WORKERS ?? 1),
  fullyParallel: true,
  retries: 0,
  reporter: [['list'], ['json']],
  use: {
    baseURL: \`http://127.0.0.1:\${port}\`,
    // The speed benchmark's renderer profile attaches to this port.
    ...(process.env.SPEED_DEVTOOLS_PORT ? { launchOptions: { args: [\`--remote-debugging-port=\${process.env.SPEED_DEVTOOLS_PORT}\`] } } : {}),
  },
  webServer: {
    command: \`npx next build && npx next start -p \${port}\`,
    url: \`http://127.0.0.1:\${port}/\`,
    cwd: '..',
    timeout: 600_000,
    reuseExistingServer: false,
    gracefulShutdown: { signal: 'SIGTERM', timeout: 10_000 },
  },
});
`);
for (let file = 0; file < Math.ceil(TESTS / PER_FILE); file += 1) {
  const tests = [];
  for (let index = 0; index < PER_FILE && file * PER_FILE + index < TESTS; index += 1) {
    const number = file * PER_FILE + index;
    const section = number % SECTIONS;
    const others = [1, 2, 3].map((step) => (section + step * 5) % SECTIONS);
    tests.push(`test('flow ${number}', async ({ page }) => {
  await page.goto('/s/${section}');
  await expect(page.getByRole('heading', { name: 'Section ${section}' })).toBeVisible();
  await page.getByRole('link', { name: 'Item ${section}-1', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Item ${section}-1' })).toBeVisible();
  await page.goBack();
  await page.getByLabel('Title').fill('Added ${number}');
  await page.getByRole('button', { name: 'Add' }).click();
  await expect(page.getByTestId('count')).toContainText('items');
${others.map((other) => `  await page.getByRole('link', { name: 'Section ${other}', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Section ${other}' })).toBeVisible();`).join('\n')}
});`);
  }
  write(`tests/flows-${String(file).padStart(2, '0')}.spec.ts`, `import { expect, test } from '@playwright/test';\n\n${tests.join('\n\n')}\n`);
}

for (const [path, text] of Object.entries(files)) {
  mkdirSync(dirname(join(directory, path)), { recursive: true });
  writeFileSync(join(directory, path), text);
}
console.log(`[large-app] ${Object.keys(files).length} files in ${directory}: ${SECTIONS} sections, ${COMPONENTS} components, ${TESTS} tests`);
