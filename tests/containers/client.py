"""Protocol fixture: only the restricted socket and token are visible."""
import json
import socket
import sys
from pathlib import Path

with socket.socket(socket.AF_UNIX) as connection:
    connection.settimeout(5)
    connection.connect('/run/agentdocker.sock')
    stream = connection.makefile('rwb')
    def request(value):
        stream.write(json.dumps(value).encode() + b'\n')
        stream.flush()
        return json.loads(stream.readline())
    if sys.argv[1] != 'unauthenticated':
        result = request({'op': 'authenticate', 'token': Path('/run/agentdocker.token').read_text().strip()})
        if result.get('type') != 'ok':
            print(json.dumps(result))
            sys.exit(0)
    print(json.dumps(request(json.loads(sys.argv[2]))))
