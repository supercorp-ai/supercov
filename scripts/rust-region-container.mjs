// A Rust region in a container is read live from the host.
//
//   node scripts/rust-region-container.mjs
//
// An instrumented program runs in a Linux container with its evidence
// directory on a bind mount; the host must see a decision it takes within a
// second, while it still runs. Docker Desktop's VM shows the host a shared
// mapping only after msync, which the runtime's flusher does when
// SUPERCOV_CONTAINER is set. Without Docker there is nothing to run, and the
// gate says so.
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const docker = spawnSync('docker', ['info', '--format', '{{.ServerVersion}}'], { encoding: 'utf8' });
if (process.platform === 'win32' || docker.status !== 0) {
  console.log('[rust-region-container] skipped: no Docker daemon here');
  process.exit(0);
}
const test = spawnSync(
  'cargo',
  ['test', '-q', '-p', 'supercov-engine', '--lib', 'a_region_in_a_container_is_read_live_from_the_host', '--', '--ignored', '--exact', 'rust_runtime::tests::a_region_in_a_container_is_read_live_from_the_host'],
  { cwd: repository, stdio: 'inherit' },
);
if (test.status !== 0) process.exit(test.status ?? 1);
console.log('[rust-region-container] a region in a container is read live from the host');
