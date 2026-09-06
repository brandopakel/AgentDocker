"""Real, separately reported engines: scoped mounts, reconnects and image validation.

The fixture owns only its daemon, labeled containers and private transport files.
"""
import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import time

p = argparse.ArgumentParser()
for name in ['engine', 'daemon', 'cli', 'context', 'root', 'result']:
    p.add_argument('--' + name, required=True)
p.add_argument('--machine')
a = p.parse_args()
root = Path(a.root).resolve()
root.mkdir(parents=True, exist_ok=False)
checkout = root / 'checkout'
checkout.mkdir()
(checkout / 'source').write_text('original')
# A tracked fixture keeps ephemeral response files out of content fingerprints.
subprocess.run(['git','init','-q',str(checkout)],check=True)
(checkout / '.gitignore').write_text('response.json\nrequest.json\nheartbeat\n')
subprocess.run(['git','-C',str(checkout),'add','.'],check=True)
subprocess.run(['git','-C',str(checkout),'-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture'],check=True)
home = root/('state-'+80*'x')
host = root/'host.sock'
log = (root/'daemon.log').open('ab')
daemon = None
owned = {}
active_container = None

def rpc(request):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(120)
        s.connect(str(host))
        s.sendall(json.dumps(request).encode()+b'\n')
        return json.loads(s.makefile('rb').readline())

def expect(response, kind):
    assert response.get('type') == kind, response
    return response

def wait(action, predicate, seconds=40):
    end=time.monotonic()+seconds
    last=None
    while time.monotonic()<end:
        try:
            last=action()
            if predicate(last): return last
        except (OSError,ValueError): pass
        time.sleep(.1)
    raise AssertionError(('timed out',last))

def start():
    global daemon
    daemon=subprocess.Popen([str(Path(a.daemon).resolve()),'--home',str(home),'--socket',str(host)],stdout=log,stderr=subprocess.STDOUT)
    wait(lambda:rpc({'op':'ping'}),lambda r:r['type']=='pong')

def inspect(agent): return expect(rpc({'op':'inspect','agent':agent}),'agent')['agent']
def engine(*args): return subprocess.check_output([a.engine,*args],timeout=30)
def remember(record):
    c=record['container']; owned[c['id'] or c['name']]=c['owner']
    return record

def launch(name,build,network='none'):
    global active_container
    # Requests go through the mounted endpoint from the actual long-running worker.
    command=['python3','-u','-c',WORKER]
    response=rpc({'op':'run_container','build':build['id'],'options':{'mount_checkout':True,'podman_machine':a.machine,'network':network},'spec':{'name':name,'workdir':str(checkout),'command':command}})
    if response['type']=='error':
        remember(inspect(name))
        raise AssertionError(response)
    record=remember(expect(response,'agent')['agent'])
    active_container = record['container']['id']
    return wait(lambda:inspect(record['id']),lambda r:r['status']['state']=='running')

def scoped(request, auth='own'):
    output=engine('container','exec',active_container,'python3','-c',CLIENT,json.dumps({'request':request,'auth':auth}))
    return json.loads(output)

CLIENT=r'''
import json,os,socket,sys
from pathlib import Path
data=json.loads(sys.argv[1])
try:
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(5);s.connect(os.environ['AGENTDOCKER_SOCKET'])
        f=s.makefile('rb')
        def call(r):
            s.sendall(json.dumps(r).encode()+b'\n')
            return json.loads(f.readline())
        mode=data['auth']
        if mode=='none': result=call(data['request'])
        else:
            token=Path(os.environ['AGENTDOCKER_TOKEN_FILE']).read_text() if mode=='own' else 'wrong'
            result=call({'op':'authenticate','token':token})
            if result['type']=='ok': result=call(data['request'])
    print(json.dumps(result))
except Exception as e:
    print(json.dumps({'transport_error':str(e)}))
'''

WORKER=r'''
import os,time
from pathlib import Path
assert os.getcwd()=='/workspace'
assert not Path('/var/run/docker.sock').exists()
assert not Path('/run/podman/podman.sock').exists()
assert set(p.name for p in Path('/run/agentdocker-auth').iterdir()) <= {'token','endpoint.sock'}
Path('heartbeat').write_text(str(time.time()))
time.sleep(600)
'''

results=[]
try:
    start()
    build=expect(rpc({'op':'build_image','spec':{'engine':a.engine,'context':str(Path(a.context).resolve()),'recipe':'Containerfile','timeout_secs':600}}),'image_build')['build']
    worker=launch('workspace-worker',build)
    wid=worker['id']
    wait(lambda:(checkout/'heartbeat').read_text(),bool)
    token=Path(worker['container']['workspace']['access']['directory'])/'token'
    assert token.stat().st_mode & 0o777 == 0o600
    assert token.read_text() not in json.dumps(worker)
    assert expect(scoped({'op':'ping'}),'pong')
    for auth in ['none','wrong']:
        assert expect(scoped({'op':'ping'},auth),'error')['code']=='forbidden'
    peer=expect(rpc({'op':'register','spec':{'name':'peer','workdir':str(checkout)}}),'agent')['agent']
    for request in [
        {'op':'inspect','agent':peer['id']}, {'op':'shutdown'},
        {'op':'claim','agent':wid,'resource':'path:/workspace/../escape'},
        {'op':'run_container','build':build['id'],'spec':{'name':'forbidden','command':['true']}},
        {'op':'validate','agent':wid,'command':['true'],'timeout_secs':1},
    ]:
        assert expect(scoped(request),'error')['code']=='forbidden'
    results.append('private mounts and authentication/admin/identity/traversal rejection')
    observe={'op':'observe','agent':wid,'paths':['/workspace/source']}
    expect(scoped(observe),'reads')
    (checkout/'source').write_text('changed by peer')
    stale=expect(scoped({'op':'stale','agent':wid,'paths':['/workspace/source']}),'stale')
    assert stale['stale'],stale
    expect(scoped(observe),'reads')
    (checkout/'alias').symlink_to('source')
    lease=expect(scoped({'op':'claim','agent':wid,'resource':'path:/workspace/alias','ttl_secs':300}),'lease')['lease']
    assert expect(rpc({'op':'claim','agent':peer['id'],'resource':'path:'+str(checkout/'source')}),'error')['code']=='conflict'
    results.append('physical alias conflicts and stale reread')
    daemon.kill();daemon.wait(timeout=20);daemon=None
    start()
    wait(lambda:inspect(wid),lambda r:r['status']['state']=='running')
    wait(lambda:scoped({'op':'ping'}),lambda r:r.get('type')=='pong')
    assert inspect(wid)['container']['id']==worker['container']['id']
    assert expect(rpc({'op':'claim','agent':peer['id'],'resource':'path:'+str(checkout/'source')}),'error')['code']=='conflict'
    results.append('daemon crash reconnects mounted endpoint and retains writer identity/lease')
    command=['python3','-c',"from pathlib import Path; assert Path('/etc/alpine-release').is_file(); assert Path('/workspace/source').read_text() == 'changed by peer'; print('image-validated')"]
    passed=expect(rpc({'op':'validate','agent':wid,'command':command,'timeout_secs':20}),'validation')
    assert passed['passed'],passed
    evidence=passed['validation']
    assert evidence['environment']['image_id']==build['image_id'] and evidence['container']['id']
    remember(inspect(evidence['container']['agent']))
    assert 'image-validated' in Path(evidence['log']).read_text()
    failed=expect(rpc({'op':'validate','agent':wid,'command':['python3','-c',"open('/workspace/source','w').write('bad')"],'timeout_secs':20}),'validation')
    assert not failed['passed'] and (checkout/'source').read_text()=='changed by peer'
    timeout=expect(rpc({'op':'validate','agent':wid,'command':['python3','-c','import time;time.sleep(60)'],'timeout_secs':1}),'validation')
    assert not timeout['passed'] and timeout['validation']['timed_out'],timeout
    results.append('validation executes image, read-only source, confirmed timeout exit')
    pool=concurrent.futures.ThreadPoolExecutor(max_workers=1)
    prior={r['id'] for r in rpc({'op':'list','all':True})['agents']}
    pending=pool.submit(rpc,{'op':'validate','agent':wid,'command':['python3','-c','import time;time.sleep(60)'],'timeout_secs':5})
    runners=wait(lambda:[r for r in rpc({'op':'list','all':True})['agents'] if r['id'] not in prior and r['status']['state']=='running'],bool)
    runner=remember(runners[0])
    daemon.kill();daemon.wait(timeout=20);daemon=None
    try: pending.result(timeout=10)
    except (OSError,ValueError,concurrent.futures.TimeoutError): pass
    pool.shutdown(wait=True)
    start()
    retired=wait(lambda:inspect(runner['id']),lambda r:r['status']['state']=='exited')
    assert retired['container']['intent']=='kill'
    incomplete=expect(rpc({'op':'validations','agent':wid}),'validations')['validations']
    assert any(v['exit_code'] is None and v['environment']['image_id']==build['image_id'] for v in incomplete)
    results.append('validation deadline survives daemon crash without fabricating passing evidence')
    checkpoint=expect(rpc({'op':'checkpoint','agent':wid,'key':'workspace','task':'continue','assumptions':[],'next_steps':[],'release_leases':False}),'checkpoint')['checkpoint']
    daemon.kill();daemon.wait(timeout=20);daemon=None
    start()
    recovery=expect(rpc({'op':'resume','agent':wid,'checkpoint':checkpoint['id'],'acknowledge':False}),'recovery')['recovery']
    assert recovery['environment_matches'] and recovery['validations'],recovery
    (checkout/'source').write_text('new unvalidated source')
    changed=expect(rpc({'op':'resume','agent':wid,'checkpoint':checkpoint['id'],'acknowledge':False}),'recovery')['recovery']
    assert not changed['checkout_matches'] and not changed['validations'],changed
    (checkout/'source').write_text('changed by peer')
    image2=root/'image2'
    shutil.copytree(Path(a.context).resolve(),image2)
    with (image2/'Containerfile').open('a') as recipe: recipe.write('\nRUN printf changed > /image-version\n')
    build2=expect(rpc({'op':'build_image','spec':{'engine':a.engine,'context':str(image2),'recipe':'Containerfile','timeout_secs':600}}),'image_build')['build']
    assert build2['image_id'] != build['image_id']
    expect(rpc({'op':'stop','agent':wid,'force':True}),'agent')
    cli=[str(Path(a.cli).resolve()),'--socket',str(host),'run','--image-build',build2['id'],'--mount-checkout','--network','bridge','--name','different-build','-w',str(checkout)]
    if a.machine: cli += ['--podman-machine',a.machine]
    cli += ['--','python3','-u','-c',WORKER]
    other_id=subprocess.check_output(cli,timeout=60,text=True).strip()
    other=remember(inspect(other_id))
    active_container=other['container']['id']
    recovery=expect(rpc({'op':'resume','agent':other['id'],'checkpoint':checkpoint['id'],'acknowledge':False}),'recovery')['recovery']
    assert not recovery['environment_matches'] and not recovery['validations'],recovery
    assert expect(rpc({'op':'resume','agent':other['id'],'checkpoint':checkpoint['id'],'acknowledge':True}),'error')['code']=='conflict'
    expect(rpc({'op':'revoke_access','grant':other['container']['workspace']['access']['grant']}),'ok')
    assert expect(scoped({'op':'ping'}),'error')['code']=='forbidden'
    results.append('image/build environment gates recovery and revocation is enforced')
    # Combine the managed engine with linked worktrees, first-write watching and handoff.
    git_context=root/'git-image'
    shutil.copytree(Path(a.context).resolve(),git_context)
    with (git_context/'Containerfile').open('a') as recipe:
        recipe.write('\nRUN apk add --no-cache git\n')
    git_spec={'engine':a.engine,'context':str(git_context),'recipe':'Containerfile','timeout_secs':600}
    git_build=expect(rpc({'op':'build_image','spec':git_spec}),'image_build')['build']
    options={'mount_checkout':True,'podman_machine':a.machine,'network':'none'}
    original=(checkout/'source').read_text()
    isolated=remember(expect(rpc({'op':'run_container','build':git_build['id'],'options':options,'spec':{
        'name':'isolated-writer','workdir':str(checkout),'isolate':True,
        'command':['python3','-u','-c',"from pathlib import Path; import time; Path('source').write_text('isolated first write'); time.sleep(600)"]}}),'agent')['agent'])
    isolated_root=Path(isolated['spec']['workdir'])
    assert not isolated_root.is_relative_to(home)
    wait(lambda:(isolated_root/'source').read_text(),lambda text:text=='isolated first write')
    assert (checkout/'source').read_text()==original
    active_container=isolated['container']['id']
    assert expect(scoped({'op':'ping'}),'pong')
    status=engine('container','exec',active_container,'git','--no-optional-locks','status','--porcelain').decode()
    assert ' M source' in status,status
    rows=wait(lambda:rpc({'op':'changes','project':str(isolated_root),'path':'source'}).get('changes',[]),
        lambda rows:any(r.get('checkout')==str(isolated_root) and r['kind']=='modified' for r in rows))
    checked=expect(rpc({'op':'validate','agent':isolated['id'],'command':['git','--no-optional-locks','status','--porcelain'],'timeout_secs':20}),'validation')
    assert checked['passed'],checked
    assert ' M source' in Path(checked['validation']['log']).read_text()
    results.append('isolated image checkout records its first write and supports Git plus read-only validation')

    rebuilt=expect(rpc({'op':'build_image','spec':git_spec}),'image_build')['build']
    assert rebuilt['id']!=git_build['id'] and rebuilt['image_id']==git_build['image_id']
    recipient=remember(expect(rpc({'op':'run_container','build':rebuilt['id'],'options':options,'spec':{
        'name':'image-recipient','workdir':str(isolated_root),'command':['python3','-u','-c',WORKER]}}),'agent')['agent'])
    held=expect(rpc({'op':'claim','agent':isolated['id'],'resource':'task:integrated-handoff','ttl_secs':300}),'lease')['lease']
    bundle=expect(rpc({'op':'handoff','agent':isolated['id'],'to':recipient['id'],'task':'finish isolated work','transfer_leases':True}),'handoff')['bundle']
    assert bundle['schema']==2 and bundle['environment']['inputs']['context_version']==git_build['context_version']
    assert bundle['environment']['image_id']==git_build['image_id']
    daemon.kill();daemon.wait(timeout=20);daemon=None
    start()
    preview=expect(rpc({'op':'resume','agent':recipient['id'],'checkpoint':bundle['id'],'acknowledge':False}),'recovery')['recovery']
    assert preview['environment_matches'] and preview['checkout_matches'],preview
    assert preview['checkpoint']['environment']==bundle['environment']
    (isolated_root/'source').write_text('changed after handoff')
    assert expect(rpc({'op':'resume','agent':recipient['id'],'checkpoint':bundle['id'],'acknowledge':True}),'error')['code']=='conflict'
    retained=expect(rpc({'op':'leases','agent':isolated['id']}),'leases')['leases']
    assert held['id'] in [lease['id'] for lease in retained]
    (isolated_root/'source').write_text('isolated first write')
    accepted=expect(rpc({'op':'resume','agent':recipient['id'],'checkpoint':bundle['id'],'acknowledge':True}),'recovery')['recovery']
    assert accepted['checkpoint']['accepted_by']==recipient['id']
    transferred=expect(rpc({'op':'leases','agent':recipient['id']}),'leases')['leases']
    assert held['id'] in [lease['id'] for lease in transferred]
    assert not expect(rpc({'op':'leases','agent':isolated['id']}),'leases')['leases']
    results.append('image handoff survives restart, accepts identical rebuilt inputs and transfers leases only after source verification')
    payload={'result':'passed','engine':a.engine,'machine':a.machine,'scenarios':results}
    Path(a.result).write_text(json.dumps(payload,indent=2)+'\n')
    print(json.dumps(payload),flush=True)
except BaseException:
    for target in owned:
        subprocess.run([a.engine,'container','logs',target],stdout=(root/('container-'+target[:16]+'.log')).open('w'),stderr=subprocess.STDOUT,check=False)
    raise
finally:
    if daemon:
        # Collect validation runners too; verify labels again before removing anything.
        try:
            for r in rpc({'op':'list','all':True}).get('agents',[]):
                if r.get('container'): remember(r)
        except (OSError,ValueError): pass
        daemon.terminate()
        try: daemon.wait(timeout=30)
        except subprocess.TimeoutExpired: daemon.kill();daemon.wait(timeout=10)
    for target,owner in owned.items():
        try:
            actual=json.loads(engine('container','inspect',target))[0]
            if actual['Config']['Labels']['org.agentdocker.owner']==owner:
                subprocess.run([a.engine,'container','rm','--force',actual['Id']],stdout=subprocess.DEVNULL,check=True,timeout=30)
        except (subprocess.SubprocessError,KeyError,ValueError): pass
    log.close()
