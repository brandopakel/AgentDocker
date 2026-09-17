#!/usr/bin/env python3
"""Actual Codex owned-bridge steering with a private local model and daemon.

No authentication, saved provider configuration or production session is used.
The local model controls busy boundaries; its output is not an LLM benchmark.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from sustained_use import FixtureProcesses, stop_daemon

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--scenario', choices=['busy', 'lost-reply', 'refused', 'changed-turn'], default='busy')
args = parser.parse_args()
os.umask(0o077)
out = args.output.resolve()
out.mkdir(mode=0o700)
binary_dir = args.binary_dir.resolve()
report = {'result':'failed', 'scope':__doc__, 'scenario':args.scenario, 'requests':[], 'checks':[],
          'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
          'source':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),
          'binary_sha256':{name:hashlib.sha256((binary_dir/name).read_bytes()).hexdigest()
                           for name in ['agentdocker','agentd']}}
releases = [threading.Event() for _ in range(3)]
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_POST(self):
  body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
  assert self.headers.get('Authorization')=='Bearer fixture-only'
  auxiliary='Generate a concise, single-line task title' in json.dumps(body.get('input',[]))
  if not auxiliary:report['requests'].append({'at':time.monotonic(),'body':body})
  n=len(report['requests']);rid='resp_fixture_'+str(n);mid='msg_fixture_'+str(n)
  if not auxiliary and n <= len(releases):releases[n-1].wait(40)
  item={'id':mid,'type':'message','role':'assistant','status':'completed','content':[{'type':'output_text','text':'FIXTURE_OK_'+str(n),'annotations':[]}]}
  response={'id':rid,'object':'response','model':'fixture-model','status':'completed','output':[item],'usage':{'input_tokens':5,'output_tokens':2,'total_tokens':7}}
  events=[{'type':'response.created','response':dict(response,status='in_progress',output=[])},{'type':'response.output_item.added','output_index':0,'item':dict(item,status='in_progress',content=[])},{'type':'response.content_part.added','item_id':mid,'output_index':0,'content_index':0,'part':{'type':'output_text','text':'','annotations':[]}},{'type':'response.output_text.delta','item_id':mid,'output_index':0,'content_index':0,'delta':'FIXTURE_OK_'+str(n)},{'type':'response.output_text.done','item_id':mid,'output_index':0,'content_index':0,'text':'FIXTURE_OK_'+str(n)},{'type':'response.output_item.done','output_index':0,'item':item},{'type':'response.completed','response':response}]
  data=''.join('event: '+e['type']+'\ndata: '+json.dumps(e)+'\n\n' for e in events).encode()
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)

def wait(fn, timeout=45):
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        value=fn()
        if value:return value
        time.sleep(.05)
    raise TimeoutError('fixture condition did not become true')

def rpc(endpoint, request):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(5)
        stream.connect(str(endpoint))
        stream.sendall(json.dumps(request).encode()+b'\n')
        with stream.makefile('rb') as reader:
            value=json.loads(reader.readline(8*1024*1024))
    if value.get('type')=='error':raise RuntimeError(value)
    return value

server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
server.daemon_threads=True
threading.Thread(target=server.serve_forever,daemon=True).start()
daemon=None
fixtures=FixtureProcesses()
started=time.monotonic()
with tempfile.TemporaryDirectory(prefix='ad-steer-bridge-',dir='/tmp') as temporary:
    root=Path(temporary).resolve()
    profile=root/'profile';profile.mkdir()
    repo=root/'project';repo.mkdir()
    state=root/'state';endpoint=root/'daemon.sock'
    cli=binary_dir/'agentdocker'
    try:
        subprocess.run(['git','init','-q',str(repo)],check=True)
        codex=shutil.which('codex')
        assert codex,'Codex is not installed'
        report['provider_version']=subprocess.check_output([codex,'--version'],text=True).strip()
        config='model = "fixture-model"\nmodel_provider = "fixture"\napproval_policy = "never"\nsandbox_mode = "read-only"\ncheck_for_update_on_startup = false\n[model_providers.fixture]\nname = "Local fixture"\nbase_url = '+json.dumps('http://127.0.0.1:'+str(server.server_port)+'/v1')+'\nwire_api = "responses"\nenv_key = "AGENTDOCKER_FIXTURE_KEY"\nrequest_max_retries = 0\nstream_max_retries = 0\nsupports_websockets = false\n[projects.'+json.dumps(str(repo))+']\ntrust_level = "trusted"\n'
        (profile/'config.toml').write_text(config)
        env={k:v for k,v in os.environ.items() if k in ['PATH','HOME','USER','LOGNAME','TMPDIR','SHELL','LANG','LC_ALL']}
        env.update(CODEX_HOME=str(profile),AGENTDOCKER_FIXTURE_KEY='fixture-only',
                   AGENTDOCKER_HOME=str(state),AGENTDOCKER_SOCKET=str(endpoint),
                   AGENTDOCKER_NO_AUTOSTART='1',AGENTDOCKER_NO_NOTIFICATIONS='1')
        tap=out/'provider-requests.jsonl'
        dropped=out/'reply-dropped'
        wrapper=root/'codex'
        wrapper.write_text('#!/usr/bin/env python3\n'+
            'import json,os,subprocess,sys,threading\n'+
            'program='+repr(codex)+'\nlog='+repr(str(tap))+'\nmarker='+repr(str(dropped))+'\ndrop='+repr(args.scenario=='lost-reply')+'\n'+
            'refuse='+repr(args.scenario=='refused')+'\nchanged='+repr(args.scenario=='changed-turn')+'\n' +
            'child=subprocess.Popen([program]+sys.argv[1:],stdin=subprocess.PIPE,stdout=subprocess.PIPE)\nsteers=set()\n'+
            'def read():\n for line in child.stdout:\n  value=json.loads(line)\n  if drop and value.get("id") in steers and "method" not in value and "result" in value and not os.path.exists(marker):\n   open(marker,"w").write(json.dumps(value));continue\n  sys.stdout.buffer.write(line);sys.stdout.buffer.flush()\n'+
            'thread=threading.Thread(target=read,daemon=True);thread.start()\n'+
            'try:\n for line in sys.stdin.buffer:\n  value=json.loads(line)\n  if value.get("method")=="turn/steer":steers.add(value["id"])\n  with open(log,"a") as writer:writer.write(json.dumps(value)+"\\n")\n  if changed and value.get("method")=="turn/steer":\n   expected=value["params"]["expectedTurnId"];actual="fixture-unexpected-turn"\n   reply={"id":value["id"],"error":{"code":-32600,"message":f"expected active turn id `{expected}` but found `{actual}`"}}\n   completed={"method":"turn/completed","params":{"threadId":value["params"]["threadId"],"turn":{"id":actual,"status":"completed"}}}\n   sys.stdout.write(json.dumps(reply)+"\\n"+json.dumps(completed)+"\\n");sys.stdout.flush()\n   open(marker,"w").write(json.dumps({"expected":expected,"reported":actual,"completion_emitted":True}));continue\n  if refuse and value.get("method")=="turn/steer":\n   reply={"id":value["id"],"error":{"code":-32600,"message":"no active turn to steer"}}\n   sys.stdout.write(json.dumps(reply)+"\\n");sys.stdout.flush();continue\n  child.stdin.write(line);child.stdin.flush()\nfinally:\n child.stdin.close()\n try:child.wait(timeout=5)\n except subprocess.TimeoutExpired:child.kill();child.wait(timeout=5)\n thread.join(timeout=2)\n')
        wrapper.chmod(0o700)
        with (out/'daemon.log').open('w') as log:
            daemon=subprocess.Popen([str(binary_dir/'agentd')],cwd=repo,env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
        def online():
            try:return rpc(endpoint,{'op':'ping'}).get('type')=='pong'
            except OSError:return False
        wait(online,15)
        human=rpc(endpoint,{'op':'me','workdir':str(repo)})['agent']['id']
        peer=rpc(endpoint,{'op':'register','spec':{'name':'fixture-peer','runtime':'fixture','workdir':str(repo)}})['agent']['id']
        run=subprocess.run([str(cli),'run','--runtime','codex','--codex-input','--name','steering-fixture','--workdir',str(repo),'--restart','no' if args.scenario=='changed-turn' else 'on-failure:1','--',str(wrapper)],cwd=repo,env=env,capture_output=True,text=True,timeout=15)
        assert run.returncode==0,run.stderr
        agent=run.stdout.strip()
        ledger_path=state/'codex-input'/agent/'delivery.json'
        def inspect():
            record=rpc(endpoint,{'op':'inspect','agent':agent})['agent']
            if record.get('pid') and record['pid'] not in fixtures.leaders:
                fixtures.remember(record['pid'])
            fixtures.capture()
            return record
        def ledger():
            inspect()
            return json.loads(ledger_path.read_text()) if ledger_path.exists() else {}
        wait(lambda:inspect().get('input_delivery',{}).get('reported_at'))
        initial=inspect()
        report['agent']=agent
        report['initial_pid']=initial['pid']
        messages=[]
        def send(sender,text,to=None):
            sent=subprocess.run([str(cli),'send','--from',sender,'--to',to or agent,text],cwd=repo,env=env,capture_output=True,text=True,timeout=10)
            assert sent.returncode==0,sent.stderr
            message=sent.stdout.splitlines()[0].strip()
            messages.append({'id':message,'text':text,'from':sender})
            return message
        first=send(human,'BUSY_START_NONCE')
        wait(lambda:len(report['requests'])==1)
        wait(lambda:ledger().get('attempt',{}).get('acknowledged'))
        second=send(peer,'PEER_BUSY_NONCE')
        wait(lambda:tap.exists() and any(json.loads(line).get('method')=='turn/steer' for line in tap.read_text().splitlines()))
        if args.scenario=='changed-turn':
            final=wait(lambda:current if (current:=inspect()).get('input_delivery',{}).get('paused') else None,20)
            reason=final['input_delivery'].get('pause_reason','')
            assert 'different active turn' in reason,reason
            assert 'unexpected turn' not in reason,reason
            assert dropped.is_file(),'changed-turn completion was not emitted'
            fault=json.loads(dropped.read_text())
            assert fault['completion_emitted'] and fault['expected']!=fault['reported'],fault
            retained=ledger()
            assert retained['steering'] is None,retained
            assert retained['attempt']['message']==first and retained['attempt']['acknowledged'],retained
            assert retained['attempt']['receipt']['turn']==fault['expected'],retained
            connection=sqlite3.connect((state/'state.db').as_uri()+'?mode=ro',uri=True)
            try:
                pending=[row[0] for row in connection.execute('SELECT message_id FROM inbox WHERE agent=? ORDER BY seq',(agent,))]
            finally:
                connection.close()
            assert pending==[second],pending
            requests=[json.loads(line) for line in tap.read_text().splitlines()]
            assert sum(r.get('method')=='turn/start' for r in requests)==1,requests
            assert sum(r.get('method')=='turn/steer' for r in requests)==1,requests
            assert len(report['requests'])==1,report['requests']
            assert final['input_delivery']['received']['messages']==[first],final
            report.update(result='passed',messages=messages,retained_messages=pending,
                          first_receipt=retained['attempt']['receipt'],pause_reason=reason,
                          changed_turn=fault,production_mutations=False,no_resubmission=True)
            report['checks']=['different-active-turn refusal pauses explicitly before processing that turn completion',
                              'original input receipt remains unchanged','unsubmitted peer message remains queued once',
                              'no receipt or second submission invented for the refused input']
        else:
            if args.scenario=='refused':
                wait(lambda:ledger().get('steering') is None)
                # Keep the real provider busy across several 500-ms queue polls.
                time.sleep(2)
                attempts=[json.loads(line) for line in tap.read_text().splitlines()]
                assert sum(r.get('method')=='turn/steer' for r in attempts)==1,attempts
                assert ledger()['attempt']['message']==first
            else:
                assert ledger()['steering']['message']==second
            releases[0].set()
            wait(lambda:len(report['requests'])==2)
            if args.scenario=='busy':
                wait(lambda:second in [entry['message'] for entry in ledger().get('completed',[])])
                third=send(human,'HUMAN_BROADCAST_PAUSE_NONCE','all')
                wait(lambda:sum(json.loads(line).get('method')=='turn/steer' for line in tap.read_text().splitlines())==2)
                releases[1].set()
                wait(lambda:len(report['requests'])==3)
                releases[2].set()
            else:
                if args.scenario=='lost-reply':assert dropped.is_file(),'steering response was not cut'
                releases[1].set()
            finished=wait(lambda:current if (current:=ledger()).get('attempt') is None and len(current.get('completed',[]))==len(messages) else None,70)
            receipts={entry['message']:entry['receipt'] for entry in finished['completed']}
            report.update(messages=messages,receipts=receipts)
            assert set(receipts)=={m['id'] for m in messages},receipts
            assert len({r['turn'] for r in receipts.values()})==(2 if args.scenario=='refused' else 1),receipts
            assert len({r['item'] for r in receipts.values()})==len(messages),receipts
            assert len(report['requests'])==len(messages),report['requests']
            for index,message in enumerate(messages):
                users=[item for item in report['requests'][index]['body']['input'] if item.get('role')=='user']
                assert message['text'] in json.dumps(users[-1]),(index,users)
            requests=[json.loads(line) for line in tap.read_text().splitlines()]
            assert sum(r.get('method')=='turn/start' for r in requests)==(2 if args.scenario=='refused' else 1),requests
            assert sum(r.get('method')=='turn/steer' for r in requests)==len(messages)-1,requests
            final=inspect()
            assert final['input_delivery']['received']['messages']==[messages[-1]['id']],final
            if args.scenario=='lost-reply':assert final['pid']!=initial['pid'],'controller did not recover'
            else:assert final['pid']==initial['pid'],'unexpected controller replacement'
            report.update(result='passed',messages=messages,receipts=receipts,final_pid=final['pid'],
                          same_turn=args.scenario!='refused',production_mutations=False,exact_receipts=True,no_resubmission=True)
            report['checks']=['CLI human and peer sends use owned input route',
                              'refused input starts once after completion' if args.scenario=='refused' else 'same active turn',
                              'exact per-message receipt','no duplicated provider submission']
            if args.scenario=='refused':report['checks'].append('one injected precondition refusal; no retry while the original turn remains active')
            if args.scenario=='lost-reply':report['checks'].append('lost steering reply reconciled after supervised controller restart')
    except BaseException as error:
        report['error']=str(error);traceback.print_exc()
    finally:
        for event in releases:event.set()
        if daemon is not None:
            try:
                cleanup=stop_daemon(daemon,endpoint,fixtures,grace_seconds=10)
                report['cleanup']=cleanup
                if cleanup.get('errors') or cleanup.get('fixtures',{}).get('remaining') or cleanup.get('fixtures',{}).get('errors'):
                    report['result']='failed'
            except BaseException as error:
                report['result']='failed';report['cleanup_error']=str(error)
        server.shutdown();server.server_close()
        report['elapsed_seconds']=time.monotonic()-started
        (out/'result.json').write_text(json.dumps(report,indent=2)+'\n')
        print(json.dumps({k:v for k,v in report.items() if k!='requests'},indent=2))
raise SystemExit(report['result']!='passed')
