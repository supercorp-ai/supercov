# CPU seconds each kind of process used while a suite ran, from /proc: the
# browser's renderers and the rest of it, the app's server, the test runner,
# the database. Written to the file it is given every half second, so the
# benchmark can stop it at any time. Processes alive when it starts count
# only what they use after.
import json
import os
import sys
import time

out = sys.argv[1]
tick = os.sysconf('SC_CLK_TCK')
start = {}
seen = {}


def kind(comm, cmd):
    if 'chrom' in comm or 'chrome' in cmd or 'headless_shell' in cmd:
        if '--type=renderer' in cmd:
            return 'browser renderer'
        if '--type=gpu-process' in cmd:
            return 'browser gpu'
        if 'network.mojom.NetworkService' in cmd:
            return 'browser network'
        if '--type=' in cmd:
            return 'browser utility'
        return 'browser main'
    if 'postgres' in comm or 'postgres' in cmd:
        return 'postgres'
    if comm.startswith('node') or comm == 'MainThread' or 'node' in cmd.split(' ')[0]:
        if 'next start' in cmd or 'next-server' in comm or 'next-server' in cmd or 'start-server' in cmd:
            return 'node next server'
        if 'next build' in cmd or 'build' in cmd.split(' ')[1:3]:
            return 'node next build'
        if 'playwright' in cmd:
            return 'node playwright'
        if 'vitest' in cmd:
            return 'node vitest'
        # Which script, by the last two parts of its path.
        args = [part for part in cmd.split(' ')[1:] if part and not part.startswith('-')]
        script = '/'.join(args[0].split('/')[-2:]) if args else '?'
        return f'node other {script}'
    if comm.startswith('supercov'):
        return 'supercov'
    return f'other {comm}'


def sample(first):
    for pid in os.listdir('/proc'):
        if not pid.isdigit():
            continue
        try:
            with open(f'/proc/{pid}/stat') as handle:
                stat = handle.read()
            comm = stat[stat.index('(') + 1:stat.rindex(')')]
            fields = stat[stat.rindex(')') + 2:].split()
            used = (int(fields[11]) + int(fields[12])) / tick
            started = fields[19]
            with open(f'/proc/{pid}/cmdline') as handle:
                cmd = ' '.join(handle.read().split('\0'))
        except Exception:
            continue
        key = (pid, started)
        if first:
            start[key] = used
        else:
            seen[key] = (kind(comm, cmd), used - start.get(key, 0))


sample(True)
while True:
    sample(False)
    totals = {}
    for what, used in seen.values():
        totals[what] = round(totals.get(what, 0) + used, 2)
    # Other processes by name, the five busiest of each; the rest as one.
    for prefix in ('other ', 'node other '):
        others = sorted((what for what in totals if what.startswith(prefix) and not what.endswith(' rest')), key=lambda what: -totals[what])
        for what in others[5:]:
            totals[prefix + 'rest'] = round(totals.get(prefix + 'rest', 0) + totals.pop(what), 2)
    with open(out + '.tmp', 'w') as handle:
        json.dump(totals, handle)
    os.replace(out + '.tmp', out)
    time.sleep(0.5)
