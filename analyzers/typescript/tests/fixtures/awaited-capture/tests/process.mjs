import { spawn as launch } from 'node:child_process';
import { setTimeout as pause } from 'node:timers/promises';

export function openWorker(context) {
  const worker = launch(process.execPath, ['src/cli.mjs'], { stdio: 'pipe' });
  let captured = '';
  let diagnostic = '';
  worker.stdout.setEncoding('utf8').on('data', data => { captured += data; });
  worker.stderr.setEncoding('utf8').on('data', data => { diagnostic += data; });
  context.after(() => worker.kill());
  const eventually = async (check, description) => {
    const until = Date.now() + 5000;
    while (!check()) {
      if (worker.exitCode !== null || worker.signalCode !== null || Date.now() > until) {
        throw Error(`${description}: ${captured} ${diagnostic}`);
      }
      await pause(5);
    }
  };
  return {
    text: () => captured,
    settled: () => eventually(() => /Listening on port/.test(captured + diagnostic), 'startup'),
  };
}
