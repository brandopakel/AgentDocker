"""Real-engine scoped transport test; never connects a container to the host socket."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import uuid

parser = argparse.ArgumentParser()
parser.add_argument('--engine', choices=['docker', 'podman'], required=True)
parser.add_argument('--host-socket', type=Path, required=True)
parser.add_argument('--engine-socket', required=True, help='Restricted socket as seen by the engine host/VM')
parser.add_argument('--root', type=Path, required=True, help='Test directory also reachable by engine bind mounts')
parser.add_argument('--image', required=True)
args = parser.parse_args()
root = args.root.resolve() / ('case-' + uuid.uuid4().hex[:8])
root.mkdir(parents=True)
checkout = root / 'checkout'
checkout.mkdir()
source = checkout / 'file.rs'
source.write_text('original\n')

def host(value):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(10)
        stream.connect(str(args.host_socket))
        stream.sendall(json.dumps(value).encode() + b'\n')
        return json.loads(stream.makefile('rb').readline())

def expect(reply, kind, code=None):
    assert reply['type'] == kind, reply
    if code:
        assert reply['code'] == code, reply
    return reply

agents = []
for role in ['reader', 'writer']:
    response = expect(host({'op':'register','spec':{'name':root.name + '-' + role,'workdir':str(checkout)}}), 'agent')
    agents.append(response['agent']['id'])
reader, writer = agents
grant = expect(host({'op':'grant_access','agent':reader,'container_root':'/workspace','ttl_secs':600}), 'access')
token = root / 'token'
fd = os.open(token, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
with os.fdopen(fd, 'w') as output:
    output.write(grant['token'] + '\n')

def container(request, authenticated=True):
    argv = [args.engine, 'run', '--rm', '--network=none', '--security-opt=no-new-privileges',
            '--cap-drop=ALL', '--security-opt=label=disable',
            '-v', f'{args.engine_socket}:/run/agentdocker.sock:ro',
            '-v', f'{token}:/run/agentdocker.token:ro',
            '-v', f'{checkout}:/workspace:rw', args.image,
            'authenticated' if authenticated else 'unauthenticated', json.dumps(request)]
    return json.loads(subprocess.check_output(argv, timeout=30))

try:
    expect(container({'op':'ping'}, False), 'error', 'forbidden')
    expect(container({'op':'shutdown'}), 'error', 'forbidden')
    expect(container({'op':'observe','agent':writer,'paths':['/workspace/file.rs']}), 'error', 'forbidden')
    expect(container({'op':'observe','agent':reader,'paths':['/workspace/../escape']}), 'error', 'forbidden')
    expect(container({'op':'observe','agent':reader,'paths':['/workspace/file.rs']}), 'reads')
    source.write_text('changed by another session\n')
    assert expect(container({'op':'stale','agent':reader}), 'stale')['stale']
    expect(container({'op':'observe','agent':reader,'paths':['/workspace/file.rs']}), 'reads')
    assert not expect(container({'op':'stale','agent':reader}), 'stale')['stale']
    claim = {'op':'claim','agent':reader,'resource':'path:/workspace/file.rs'}
    expect(container(claim), 'lease')
    expect(host({'op':'claim','agent':writer,'resource':'path:' + str(source)}), 'error', 'conflict')
    expect(host({'op':'revoke_access','grant':grant['grant']}), 'ok')
    expect(container({'op':'ping'}), 'error', 'forbidden')
    # Revocation must not free the still-running reader's physical protection.
    expect(host({'op':'claim','agent':writer,'resource':'path:' + str(source)}), 'error', 'conflict')
    image = subprocess.check_output([args.engine, 'image', 'inspect', args.image, '--format', '{{.Id}}'], text=True).strip()
    print(json.dumps({'engine':args.engine,'image_id':image,'result':'passed',
                      'scenarios':['authentication','identity','traversal','host-admin denial','read/change/stale/reread','physical alias conflict','revocation retains leases']}))
finally:
    host({'op':'revoke_access','grant':grant['grant']})
    for agent in agents:
        host({'op':'deregister','agent':agent})
    token.unlink(missing_ok=True)
