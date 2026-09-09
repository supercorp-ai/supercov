import { spawn } from 'node:child_process';

export async function capture() {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: ['ignore', 'pipe', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', chunk => { stderr += chunk; });
  const code = await new Promise((resolve, reject) => {
    child.on('error', reject);
    child.on('close', resolve);
  });
  return { code, stderr };
}
