// Assess a measured run's assertions against the local Jev stand-in, for a
// gate that should read its project through `assertions assess` too: every
// statement the run executed is listed, asked about and answered, with no key
// and no network.
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');

/** Runs `supercov runs latest assertions assess` in `cwd`; its output. */
export async function assessWithStandIn(binary, cwd, env = {}) {
  const directory = mkdtempSync(resolve(tmpdir(), 'supercov-assess-'));
  const child = spawn(process.execPath, [resolve(repository, 'scripts/jev-stand-in.mjs')], {
    env: { ...process.env, ANSWER: 'yes', SEEN: resolve(directory, 'seen') },
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  try {
    const port = await new Promise((settle, fail) => {
      child.stdout.once('data', (chunk) => settle(String(chunk).trim()));
      child.once('error', fail);
      child.once('exit', (code) => fail(new Error(`the stand-in exited with ${code}`)));
    });
    const assessed = spawnSync(binary, ['runs', 'latest', 'assertions', 'assess'], {
      cwd,
      encoding: 'utf8',
      env: {
        ...process.env,
        ...env,
        NO_COLOR: '1',
        TYPESAFE_BASE_URL: `http://127.0.0.1:${port}`,
        TYPESAFE_API_KEY: 'sk-stand-in',
        TYPESAFE_DEFAULT_MODEL: 'typesafe/jev-1.13',
      },
      timeout: 600_000,
      maxBuffer: 64 * 1024 * 1024,
    });
    return { status: assessed.status, output: `${assessed.stdout}${assessed.stderr}` };
  } finally {
    child.kill();
    rmSync(directory, { recursive: true, force: true });
  }
}
