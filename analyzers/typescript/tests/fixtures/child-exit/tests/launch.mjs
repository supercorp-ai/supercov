import { spawn } from 'node:child_process';

// The helper keeps one child's event arguments in a fresh result object.
export function launch() {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  return { child, exited };
}
