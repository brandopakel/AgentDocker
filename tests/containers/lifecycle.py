"""Real engine lifecycle: lost create reply, daemon crashes, outage, stop and replacement.

Owns its daemon processes and containers. Never mounts a host or engine socket.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('--engine', choices=['docker', 'podman'], required=True)
parser.add_argument('--daemon', type=Path, required=True)
parser.add_argument('--cli', type=Path, required=True)
parser.add_argument('--context', type=Path, required=True)
parser.add_argument('--root', type=Path, required=True)
parser.add_argument('--result', type=Path, required=True)
args = parser.parse_args()
root = args.root.resolve()
root.mkdir(parents=True, exist_ok=False)
host_socket = root / 'host.sock'
engine_binary = shutil.which(args.engine)
assert engine_binary
wrapper = root / 'bin'
wrapper.mkdir()
outage = root / 'outage'
lost_create = root / 'lost-create'
hold_recovery = root / 'hold-recovery'
engine_wrapper = wrapper / args.engine
engine_wrapper.write_text('''#!/usr/bin/env python3
import pathlib, subprocess, sys
root = pathlib.Path(__file__).resolve().parent.parent
if (root/'outage').exists():
    print('simulated engine outage', file=sys.stderr)
    sys.exit(69)
if sys.argv[1:3] == ['container','inspect'] and (root/'hold-recovery').exists():
    print('holding recovery until crash', file=sys.stderr)
    sys.exit(69)
real = ''' + repr(engine_binary) + '''
if sys.argv[1:3] == ['container','create'] and (root/'lost-create').exists():
    result = subprocess.run([real, *sys.argv[1:]], stdout=subprocess.PIPE)
    (root/'lost-create').unlink()
    print('simulated lost create reply', file=sys.stderr)
    sys.exit(1 if result.returncode == 0 else result.returncode)
sys.exit(subprocess.call([real, *sys.argv[1:]]))
''')
engine_wrapper.chmod(0o700)
env = dict(os.environ, PATH=str(wrapper) + os.pathsep + os.environ['PATH'], AGENTDOCKER_NO_AUTOSTART='1')
process = None
log = (root / 'daemon.log').open('ab')
owned = {}


def rpc(value):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(90)
        stream.connect(str(host_socket))
        stream.sendall(json.dumps(value).encode() + b'\n')
        return json.loads(stream.makefile('rb').readline())


def expect(value, kind):
    assert value['type'] == kind, value
    return value


def wait_for(action, predicate, seconds=30):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            last = action()
            if predicate(last):
                return last
        except (OSError, ValueError):
            pass
        time.sleep(0.1)
    raise AssertionError(('condition timed out', last))


def start():
    global process
    process = subprocess.Popen([str(args.daemon.resolve()), '--home', str(root/'state'), '--socket', str(host_socket)],
                               env=env, stdout=log, stderr=subprocess.STDOUT)
    wait_for(lambda: rpc({'op': 'ping'}), lambda r: r['type'] == 'pong')


def crash():
    global process
    process.kill()
    assert process.wait(timeout=20) != 0
    process = None


def inspect(agent):
    return expect(rpc({'op': 'inspect', 'agent': agent}), 'agent')['agent']


def engine_inspect(target):
    return json.loads(subprocess.check_output([engine_binary, 'container', 'inspect', target], timeout=30))[0]


def remember(record):
    container = record['container']
    actual = engine_inspect(container['id'] or container['name'])
    assert actual['Config']['Labels']['org.agentdocker.owner'] == container['owner']
    owned[actual['Id']] = container['owner']
    return actual['Id']


try:
    start()
    build = expect(rpc({'op': 'build_image', 'spec': {'engine': args.engine, 'context': str(args.context.resolve()),
                                                  'recipe': 'Containerfile', 'timeout_secs': 600}}), 'image_build')['build']
    command = ['python3', '-u', '-c',
               "import os,signal,time; assert not os.path.exists('/run/agentdocker.sock'); "
               "signal.signal(signal.SIGTERM,signal.SIG_IGN); print('managed-ready',flush=True); time.sleep(120)"]
    hold_recovery.touch()
    lost_create.touch()
    failed = expect(rpc({'op': 'run_container', 'build': build['id'], 'spec': {'name': 'worker', 'command': command,
                                                                                     'workdir': str(root)}}), 'error')
    assert failed['code'] == 'engine_unavailable', failed
    pending = inspect('worker')
    assert pending['status']['state'] == 'created' and pending['container']['id'] is None
    original_id = remember(pending)
    assert not engine_inspect(original_id)['State']['Running']
    crash()
    hold_recovery.unlink()
    start()
    running = wait_for(lambda: inspect(pending['id']), lambda r: r['status']['state'] == 'running')
    assert running['container']['id'] == original_id
    assert running.get('pid') is None and running.get('process_group') is None
    lease = expect(rpc({'op': 'claim', 'agent': running['id'], 'resource': 'task:lifecycle', 'ttl_secs': 600}), 'lease')['lease']
    writer = expect(rpc({'op': 'register', 'spec': {'name': 'writer'}}), 'agent')['agent']

    def conflict():
        response = expect(rpc({'op': 'claim', 'agent': writer['id'], 'resource': 'task:lifecycle'}), 'error')
        assert response['code'] == 'conflict', response

    conflict()
    crash()
    outage.touch()
    start()
    uncertain = wait_for(lambda: inspect(running['id']), lambda r: r['container']['last_error'] is not None)
    assert uncertain['status']['state'] == 'running'
    assert engine_inspect(original_id)['State']['Running']
    conflict()
    stopped = expect(rpc({'op': 'stop', 'agent': running['id'], 'force': True}), 'error')
    assert stopped['code'] == 'engine_unavailable'
    stopping = inspect(running['id'])
    assert stopping['status']['state'] == 'stopping' and stopping['container']['intent'] == 'kill'
    conflict()
    crash()
    outage.unlink()
    start()
    exited = wait_for(lambda: inspect(running['id']), lambda r: r['status']['state'] == 'exited')
    assert not engine_inspect(original_id)['State']['Running']
    leases = expect(rpc({'op': 'leases', 'agent': running['id']}), 'leases')['leases']
    assert not leases
    released = expect(rpc({'op': 'claim', 'agent': writer['id'], 'resource': 'task:lifecycle'}), 'lease')['lease']
    expect(rpc({'op': 'release', 'agent': writer['id'], 'lease': released['id']}), 'lease')
    restarted_id = subprocess.check_output([str(args.cli.resolve()), '--socket', str(host_socket), 'restart', running['id']],
                                           env=env, text=True, timeout=90).strip()
    replacement = inspect(restarted_id)
    replacement_id = remember(replacement)
    assert replacement['id'] != running['id'] and replacement_id != original_id
    assert replacement['container']['image_id'] == running['container']['image_id']
    def logs():
        return subprocess.check_output([str(args.cli.resolve()), '--socket', str(host_socket), 'logs', restarted_id],
                                       env=env, text=True, timeout=30)
    wait_for(logs, lambda text: 'managed-ready' in text)
    expect(rpc({'op': 'stop', 'agent': restarted_id}), 'agent')
    replacement_exit = wait_for(lambda: inspect(restarted_id), lambda r: r['status']['state'] == 'exited')
    assert not engine_inspect(replacement_id)['State']['Running']
    once_id = subprocess.check_output([str(args.cli.resolve()), '--socket', str(host_socket), 'run', '--image-build', build['id'],
                                       '--name', 'once', '--', 'python3', '-c', "print('once')"], env=env, text=True, timeout=90).strip()
    once = wait_for(lambda: inspect(once_id), lambda r: r['status']['state'] == 'exited')
    remember(once)
    assert once['status']['code'] == 0
    result = {'engine': args.engine, 'result': 'passed', 'build': build, 'original': exited,
              'replacement': replacement_exit, 'lease': lease['id'],
              'scenarios': ['lost-create response', 'recovery by owned identity', 'daemon crash with running container',
                            'engine outage retains leases', 'durable kill intent across restart', 'confirmed exit frees leases',
                            'restart creates new identity from same image', 'logs snapshot', 'graceful-stop escalation', 'CLI run and natural exit']}
    args.result.parent.mkdir(parents=True, exist_ok=True)
    args.result.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'engine': args.engine, 'result': 'passed', 'scenarios': result['scenarios']}))
finally:
    outage.unlink(missing_ok=True)
    if process is not None:
        process.terminate()
        try:
            process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=20)
    existing = subprocess.check_output([engine_binary, 'ps', '--all', '--no-trunc', '--format', '{{.ID}}'], text=True, timeout=30).splitlines()
    for container_id, owner in owned.items():
        if container_id not in existing:
            continue
        actual = engine_inspect(container_id)
        assert actual['Config']['Labels']['org.agentdocker.owner'] == owner
        subprocess.run([engine_binary, 'container', 'rm', '--force', container_id], check=True, timeout=30,
                       stdout=subprocess.DEVNULL)
    log.close()
