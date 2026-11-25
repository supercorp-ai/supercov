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
