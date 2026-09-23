import assert from 'node:assert/strict';
import test from 'node:test';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';

const runtime = resolve(import.meta.dirname, '../../runtime/python');
const candidates = process.env.SUPERCOV_PYTHON ? [process.env.SUPERCOV_PYTHON] : ['python3.14', 'python3.13', 'python3.12', 'python3.11', 'python3.10', 'python3.9', 'python3'];
const python = candidates.find(p => spawnSync(p, ['-c', 'import sys; sys.exit(sys.version_info < (3, 9))']).status === 0);
const skip = python ? false : 'CPython 3.9+ is unavailable';
function run(script) {
  const result = spawnSync(python, ['-c', script, runtime], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stdout + result.stderr);
}

const fixture = `
import os, sys, json, tempfile, pathlib, subprocess
sys.path.insert(0, sys.argv[1])
import supercov_probes as probes
source = 'def choose(a, b):\\n    if a and b:\\n        return "yes"\\n    return "no"\\n'
def file_plan(name):
    return {"statements": [{"id": name + ':s', "start": [1,0], "end": [4,15], "lines": [1,4]}],
            "decisions": [{"id": name + ':d', "span": [[2,7],[2,14]], "outcomeTrue": name + ':t', "outcomeFalse": name + ':f',
                           "conditions": [{"span": [[2,7],[2,8]], "not": 0}, {"span": [[2,13],[2,14]], "not": 0}]}]}
def child(code, root, env):
    p = subprocess.run([sys.executable, '-c', code], cwd=root, env=env, text=True, capture_output=True)
    assert p.returncode == 0, (p.stdout, p.stderr)
    assert '[supercov]' not in p.stderr, p.stderr
    return p.stdout
`;

test('ordinary compilation stays plain and exported probes preserve condition semantics without the runtime', { skip }, () => run(fixture + `
with tempfile.TemporaryDirectory() as temporary:
    root = pathlib.Path(temporary)
    names = ['compiled', 'explicit', 'loaded', 'exported']
    for name in names: (root / (name + '.py')).write_text(source)
    plan = root / 'plan.json'
    plan.write_text(json.dumps({'version': 1, 'root': str(root), 'files': {name + '.py': file_plan(name) for name in names}}))
    plain = {k:v for k,v in os.environ.items() if not k.startswith('SUPERCOV_') and k != 'PYTHONPATH'}
    env = dict(plain, PYTHONPATH=sys.argv[1], SUPERCOV_PYTHON_PLAN=str(plan), SUPERCOV_RUN_ID='cache', SUPERCOV_PYTHON_EVIDENCE_DIR=str(root / 'evidence'))
    child('''import py_compile, importlib.machinery, importlib.util, marshal, pathlib
py_compile.compile('compiled.py', doraise=True)
py_compile.compile('explicit.py', cfile='explicit.pyc', doraise=True)
code = importlib.machinery.SourceFileLoader('loaded', 'loaded.py').get_code('loaded')
namespace = {}; exec(code, namespace)
assert namespace['choose'](True, True) == 'yes'
assert not pathlib.Path(importlib.util.cache_from_source('loaded.py')).exists()
for path in [importlib.util.cache_from_source('compiled.py'), 'explicit.pyc']:
    data = pathlib.Path(path).read_bytes()
    assert b'supercov_probes' not in data, path
# An external consumer can persist compile()'s code itself. Its fallback
# must still preserve semantics without an installed Supercov module.
code = compile(pathlib.Path('exported.py').read_text(), str(pathlib.Path('exported.py').resolve()), 'exec')
pathlib.Path('exported.code').write_bytes(marshal.dumps(code))
''', root, env)
    child('''import compiled, loaded, marshal, pathlib, sys
from importlib.machinery import SourcelessFileLoader
explicit = SourcelessFileLoader('explicit', 'explicit.pyc').load_module()
namespace = {}; exec(marshal.loads(pathlib.Path('exported.code').read_bytes()), namespace)
namespace['bool'] = lambda value: False  # Source globals must not alter fallback truth conversion.
for choose in [compiled.choose, loaded.choose, explicit.choose, namespace['choose']]:
    assert [choose(a,b) for a,b in [(False,False),(False,True),(True,False),(True,True)]] == ['no','no','no','yes']
assert 'supercov_probes' not in sys.modules
''', root, plain)
    child('''import marshal, pathlib
namespace = {}; exec(marshal.loads(pathlib.Path('exported.code').read_bytes()), namespace)
assert namespace['choose'](True, True) == 'yes'
assert namespace['choose'](True, False) == 'no'
''', root, dict(plain, PYTHONPATH=sys.argv[1]))
`));

test('prepared plans load lazily, preserve numbering, invalidate on change and recover from truncated caches', { skip }, () => run(fixture + `
with tempfile.TemporaryDirectory() as temporary:
    root = pathlib.Path(temporary); path = root / 'plan.json'
    plan = {'version': 1, 'root': str(root), 'files': {name + '.py': file_plan(name) for name in ['a','b']}, 'assertionSites': {'test_a.py': [3,4]}}
    path.write_text(json.dumps(plan))
    _, cold = probes.load_plan(str(path), 1)
    original = probes.index_plan
    probes.index_plan = lambda plan: (_ for _ in ()).throw(AssertionError('warm startup indexed the plan'))
    _, warm = probes.load_plan(str(path), 1)
    assert not warm.files.loaded and not warm.ids.loaded and not warm.decisions.loaded and not warm.boolop_groups.loaded
    assert 'a.py' in warm.files and not warm.files.loaded
    assert warm.files['b.py'].__dict__ == cold.files['b.py'].__dict__
    assert set(warm.files.loaded) == {'b.py'}
    assert warm.sites['test_a.py'].__dict__ == cold.sites['test_a.py'].__dict__
    assert warm.layout() == cold.layout()
    probes.index_plan = original
    plan['files']['c.py'] = file_plan('c'); path.write_text(json.dumps(plan))
    _, changed = probes.load_plan(str(path), 1)
    assert changed.digest != cold.digest and len(changed.files) == 3
    for cache in root.glob('plan.json.*'): cache.write_bytes(b'bad')
    _, recovered = probes.load_plan(str(path), 1)
    assert recovered.layout() == changed.layout()
    for cache in root.glob('plan.json.*'): cache.unlink()
    original_mkstemp = probes.tempfile.mkstemp
    def unwritable(*args, **kwargs): raise PermissionError('read-only cache')
    probes.tempfile.mkstemp = unwritable
    _, uncached = probes.load_plan(str(path), 1)
    assert uncached.layout() == changed.layout()
    probes.tempfile.mkstemp = original_mkstemp
    code = 'import sys;sys.path.insert(0,sys.argv[1]);import supercov_probes as p;print(p.load_plan(sys.argv[2],1)[1].digest)'
    children = [subprocess.Popen([sys.executable, '-c', code, sys.argv[1], str(path)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for _ in range(4)]
    for process in children:
        out, err = process.communicate()
        assert process.returncode == 0 and out.strip() == changed.digest, (out,err)
    assert probes.load_plan(str(path), 1)[1].layout() == changed.layout()
`));

test('runtime descriptors are close-on-exec; a reused descriptor is not the original launch file', { skip: skip || process.platform === 'win32' }, () => run(fixture + `
with tempfile.TemporaryDirectory() as temporary:
    root = pathlib.Path(temporary); plan = root / 'plan.json'
    plan.write_text(json.dumps({'version':1,'root':str(root),'files':{'a.py':file_plan('a')}}))
    sentinel = root / 'launch'; sentinel.write_text('launch')
    plain = {k:v for k,v in os.environ.items() if not k.startswith('SUPERCOV_') and k != 'PYTHONPATH'}
    env = dict(plain, PYTHONPATH=sys.argv[1], SUPERCOV_PYTHON_PLAN=str(plan), SUPERCOV_RUN_ID='fd', SUPERCOV_PYTHON_EVIDENCE_DIR=str(root / 'evidence'), SUPERCOV_CONTEXT=json.dumps({'test':'t','phase':'call'}))
    inspect = '''import os, supercov_runtime as rt
expected = os.stat('launch')
for fd in range(4, 64):
    try: actual = os.fstat(fd)
    except OSError: continue
    assert (actual.st_dev, actual.st_ino) != (expected.st_dev, expected.st_ino), fd
    assert not os.get_inheritable(fd), fd
runtime = rt.runtime()
assert runtime.current_identity()['test'] == 't'
assert all(not os.get_inheritable(slot.descriptor) for slot in runtime.live_slots)
'''
    launch = 'import os, sys; keeper=os.open(os.devnull,os.O_RDONLY); os.dup2(keeper,3,inheritable=True); os.set_inheritable(3,True); fd=os.open("launch",os.O_RDONLY); os.dup2(fd,9,inheritable=False); os.execve(sys.executable,[sys.executable,"-c",' + repr(inspect) + '], ' + repr(env) + ')'
    child(launch, root, plain)
`));

test('stdlib-only children preserve stdout, stderr and exit status', { skip }, () => run(fixture + `
with tempfile.TemporaryDirectory() as temporary:
    root = pathlib.Path(temporary); plan = root / 'plan.json'
    plan.write_text(json.dumps({'version':1,'root':str(root),'files':{'a.py':file_plan('a')}}))
    plain = {k:v for k,v in os.environ.items() if not k.startswith('SUPERCOV_') and k != 'PYTHONPATH'}
    env = dict(plain, PYTHONPATH=sys.argv[1], SUPERCOV_PYTHON_PLAN=str(plan), SUPERCOV_RUN_ID='streams', SUPERCOV_PYTHON_EVIDENCE_DIR=str(root / 'evidence'))
    for code in ['pass', 'print("ready")', 'import sys;sys.stderr.write("expected stderr\\\\n");sys.exit(3)']:
        baseline = subprocess.run([sys.executable,'-c',code], env=plain, cwd=root, capture_output=True)
        measured = subprocess.run([sys.executable,'-c',code], env=env, cwd=root, capture_output=True)
        assert (measured.returncode, measured.stdout, measured.stderr) == (baseline.returncode, baseline.stdout, baseline.stderr), (code, measured.stderr)
    child('''import os,sys,subprocess,supercov_runtime
supercov_runtime.runtime().switch({'test':'isolated-env','phase':'call'})
environment = {k:v for k,v in os.environ.items() if not k.startswith('SUPERCOV_') and k != 'PYTHONPATH'}
code = 'import os; assert not any(k.startswith("SUPERCOV_") for k in os.environ)'
result = subprocess.run([sys.executable,'-c',code], env=environment, capture_output=True)
assert result.returncode == 0, result.stderr
assert result.stdout == result.stderr == b''
''', root, env)
`));
