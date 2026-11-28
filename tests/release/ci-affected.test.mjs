// What a pull request's Self Coverage measures beside the affected tests:
// every test written in a file the change touched, a test it added included,
// by the name libtest runs it by.
import assert from 'node:assert/strict';
import test from 'node:test';
import { rustTests, testsInFiles } from '../../scripts/ci-affected.mjs';

test('a Rust file names its tests by their path in the test binary', () => {
  const source = [
    '// #[test] fn commented() {}',
    'const S: &str = "#[test] fn quoted() {}";',
    "fn helper<'a>(x: &'a str) -> char { '{' }",
    '#[test]',
    'fn top() {}',
    '#[cfg(test)]',
    'mod tests {',
    '    use super::*;',
    '    #[test]',
    '    fn added() { let r = r#"}"#; }',
    '    mod deeper {',
    '        #[tokio::test]',
    '        async fn awaits() {}',
    '        fn not_a_test() {}',
    '    }',
    '    #[test]',
    '    #[should_panic(expected = "no")]',
    '    fn panics() {}',
    '}',
    '/* #[test] fn hidden() { */',
    '#[test]',
    'fn after() {}',
  ].join('\n');
  assert.deepEqual(rustTests('crates/engine/src/run/mod.rs', source), [
    'run::top',
    'run::tests::added',
    'run::tests::deeper::awaits',
    'run::tests::panics',
    'run::after',
  ]);
  assert.deepEqual(rustTests('crates/cli/tests/cli.rs', '#[test]\nfn new_one() {}\n'), ['new_one']);
  assert.deepEqual(rustTests('crates/cli/src/main.rs', 'mod tests {\n#[test]\nfn t() {}\n}\n'), ['tests::t']);
  assert.deepEqual(rustTests('crates/cli/src/bin/tool.rs', '#[test]\nfn t() {}\n'), ['t']);
  assert.deepEqual(rustTests('crates/cli/tests/common/mod.rs', '#[test]\nfn t() {}\n'), ['common::t']);
  assert.deepEqual(rustTests('crates/engine/build.rs', '#[test]\nfn t() {}\n'), []);
});

test('the changed files give their tests, and a removed file none', () => {
  const files = {
    'crates/engine/src/plan.rs': 'pub fn plan() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn plans() {}\n}\n',
    'crates/cli/tests/cli.rs': '#[test]\nfn continues_after_a_failed_test() {}\n',
    'crates/supercov-cli/tests/e2e_gates.rs': 'gates! {\n    python => "scripts/python-integration.mjs",\n}\n',
  };
  const read = (file) => {
    if (!(file in files)) throw new Error('gone');
    return files[file];
  };
  assert.deepEqual(
    testsInFiles([...Object.keys(files), 'crates/engine/src/gone.rs', 'docs/cli.md'], read),
    ['plan::tests::plans', 'continues_after_a_failed_test', 'python'],
  );
});

test('tests are dealt into parts of about equal seconds, the same way in every job', async () => {
  const { deal, nextestFilter, partsFor } = await import('../../scripts/ci-affected.mjs');
  const items = [5, 1, 9, 3, 3, 7, 2].map((cost, index) => ({ key: `t${index}`, cost }));
  const piles = deal(items, 3);
  assert.deepEqual(piles.map((pile) => pile.reduce((sum, item) => sum + item.cost, 0)), [10, 10, 10]);
  assert.deepEqual(deal([...items].reverse(), 3), piles);
  // The first part starts with what it runs besides: the doctests.
  assert.equal(deal(items, 2, 30)[0].length, 0);
  assert.deepEqual(deal(items, 1, 30), [[...items].sort((a, b) => b.cost - a.cost || (a.key < b.key ? -1 : 1))]);

  const timing = new Map([['slow', 2000], ['mid', 1200], ['quick', 2]]);
  assert.equal(partsFor(['quick'], timing), 1);
  assert.equal(partsFor(Array.from({ length: 2000 }, (_, index) => `untimed_${index}`), timing), 6);

  const listing = {
    'rust-suites': {
      'supercov::cli': { 'binary-name': 'cli', kind: 'test', cwd: '/repo/crates/supercov-cli', testcases: { slow: {}, mid: {}, quick: {}, off: { ignored: true } } },
      'supercov-engine': { 'binary-name': 'supercov_engine', kind: 'lib', cwd: '/repo/crates/supercov-engine', testcases: { 'run::tests::unit': {} } },
    },
  };
  const selection = { names: ['slow', 'mid', 'quick'], changed: [] };
  const one = nextestFilter(selection, listing, '/repo', { timing });
  assert.deepEqual([one.picked, one.expression], [3, 'binary_id(supercov::cli)']);
  const parts = [1, 2].map((part) => nextestFilter(selection, listing, '/repo', { part, parts: 2, timing }));
  assert.deepEqual(parts.map((part) => part.picked).sort(), [1, 2]);
  assert.equal(parts.reduce((sum, part) => sum + part.picked, 0), 3);
  const every = [1, 2, 3].map((part) => nextestFilter({ names: [], changed: [] }, listing, '/repo', { part, parts: 3, all: true, timing }));
  assert.equal(every.reduce((sum, part) => sum + part.picked, 0), 4);
  assert.ok(every.some((part) => part.expression.includes('binary_id(supercov-engine)')));
});

test('the ready label asks the slow tier for its workflows, not CI for a second run', async () => {
  const { triage } = await import('../../scripts/ci-affected.mjs');
  const labelled = { action: 'labeled', label: { name: 'ready' }, pull_request: { title: 'x', head: { ref: 'x' }, labels: [{ name: 'ready' }] } };
  assert.equal(triage('pull_request', labelled, ['crates/x.rs'], 'ci').mode, 'skip');
  for (const workflow of ['self-coverage', 'compatibility', 'react', 'next']) assert.equal(triage('pull_request', labelled, ['crates/x.rs'], workflow).mode, 'map');
});

test('a workflow file runs in full only the workflows that run by it', async () => {
  const { triage } = await import('../../scripts/ci-affected.mjs');
  const push = { action: 'synchronize', pull_request: { title: 'x', head: { ref: 'x' }, labels: [] } };
  const mode = (changed, workflow) => triage('pull_request', push, changed, workflow).mode;
  // Another workflow's file, or the seconds the jobs deal by, leaves every tier to the map.
  for (const workflow of ['ci', 'self-coverage', 'compatibility', 'react', 'next']) {
    assert.equal(mode(['.github/workflows/mutation-jvm.yml', 'crates/x.rs'], workflow), 'map');
    assert.equal(mode(['.github/gate-seconds.tsv'], workflow), 'map');
    // What every workflow runs by still runs everything.
    assert.equal(mode(['.github/actions/affected/action.yml'], workflow), 'full');
    assert.equal(mode(['scripts/ci-affected.mjs'], workflow), 'full');
  }
  assert.equal(mode(['.github/workflows/ci.yml'], 'ci'), 'full');
  assert.equal(mode(['.github/workflows/ci.yml'], 'self-coverage'), 'map');
  assert.equal(mode(['.github/workflows/self-coverage.yml'], 'self-coverage'), 'full');
  assert.equal(mode(['.github/workflows/self-coverage.yml'], 'compatibility'), 'map');
  // The slow tier calls the other four: its file runs them all.
  for (const workflow of ['self-coverage', 'compatibility', 'react', 'next']) assert.equal(mode(['.github/workflows/slow-tier.yml'], workflow), 'full');
  assert.equal(mode(['.github/workflows/slow-tier.yml'], 'ci'), 'map');
  // Asked for no workflow, any workflow file still runs everything.
  assert.equal(mode(['.github/workflows/mutation-jvm.yml']), 'full');
});

test("the merge queue's commit runs as its pull request does, on what the group changes", async () => {
  const { triage } = await import('../../scripts/ci-affected.mjs');
  const group = (fields = {}) => ({
    action: 'checks_requested',
    merge_group: { head_ref: 'refs/heads/gh-readonly-queue/main/pr-355-0123abcd', base_sha: 'b', head_sha: 'h' },
    pull_request: { title: 'x', head: { ref: 'x' }, labels: [], ...fields },
  });
  for (const workflow of ['ci', 'self-coverage', 'compatibility', 'react', 'next']) {
    assert.deepEqual(triage('merge_group', group(), ['crates/x.rs'], workflow), { platforms: 'linux', mode: 'map', reason: '' });
    assert.equal(triage('merge_group', group(), ['scripts/ci-affected.mjs'], workflow).mode, 'full');
    assert.equal(triage('merge_group', group({ title: 'Release 4.30.0' }), ['crates/x.rs'], workflow).platforms, 'all');
    assert.equal(triage('merge_group', group({ labels: [{ name: 'full-ci' }] }), ['crates/x.rs'], workflow).mode, 'full');
    // Changed files it could not list run everything.
    assert.equal(triage('merge_group', group(), null, workflow).mode, 'full');
  }
});
