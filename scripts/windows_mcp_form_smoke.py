"""Native Windows managed Codex form/URL review on extracted release binaries.
Private stdio MCP and a loopback model exercise four decisions and exact receipts.

No account/auth files, external requests, browser navigation, installed services,
physical interaction or production configuration is used. Synthetic human answers
exercise consent receipts; they do not establish real-account/browser acceptance.
"""
import argparse, csv, datetime, hashlib, io, json, os, subprocess, sys, tempfile, threading, time, traceback, uuid
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
sys.path.insert(0, str(Path(__file__).resolve().parent))
from windows_remote_receiver_fixture import Receiver
from windows_smoke_pipe import WindowsSmokePipe
from windows_native_codex_smoke import current_user_objects, read_shared_file, read_snapshot, remove_fixture, response_events

parser=argparse.ArgumentParser();parser.add_argument('--binary-dir',type=Path,required=True);parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--codex',type=Path,required=True)
parser.add_argument('--mode',choices=('form','url'),default='form')
parser.add_argument('--idle',action='store_true',help='elicit only after the initial tool and model turn have completed')
a=parser.parse_args()
tool_name='arm_late_review' if a.idle else 'review_with_'+a.mode
mcp_driver='mcp_idle_fixture_server.py' if a.idle else 'mcp_form_fixture_server.py'
if os.name != 'nt':parser.error('requires native Windows')
import psutil
current_user_objects()
out=a.output.resolve();out.mkdir(mode=0o700)
bin=a.binary_dir.resolve(strict=True);cli=bin/'agentdocker.exe';codex=a.codex.resolve(strict=True)
evidence=Path(__file__).resolve().parent
provenance=json.loads((bin/'build.json').read_text(encoding='utf-8'))
assert provenance['source_dirty'] is False and provenance['target']=='x86_64-pc-windows-msvc'
report={'result':'failed','scope':__doc__,'started_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'source_commit':provenance['source_commit'],
 'source_tree':provenance['source_tree'],
 'source_dirty':False,
 'binary_sha256':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in [cli,bin/'agentd.exe']},
 'provider_version':subprocess.check_output([str(codex),'--version'],text=True,timeout=10).strip(),
 'provider_sha256':hashlib.sha256(codex.read_bytes()).hexdigest(),
 'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
 'mcp_driver_sha256':hashlib.sha256((evidence/mcp_driver).read_bytes()).hexdigest(),'review_mode':a.mode,'idle_review':a.idle,'cases':[],'model_requests':[]}
assert all(provenance['binary_sha256'][k]==v for k,v in report['binary_sha256'].items())
report['helper_sha256']={name:hashlib.sha256((evidence/name).read_bytes()).hexdigest() for name in ['windows_smoke_pipe.py','windows_remote_receiver_fixture.py','windows_native_codex_smoke.py']}
report['cleanup_errors']=[]
report['source_provenance']=provenance
context={'tool_sent':False,'case':None};lock=threading.Lock()

def call_events(number,body):
    found=[]
    for tool in body.get('tools',[]):
        if tool.get('type')=='function' and tool_name in tool.get('name',''):found.append((None,tool['name']))
        if tool.get('type')=='namespace':
            for member in tool.get('tools',[]):
                if tool_name in member.get('name',''):found.append((tool['name'],member['name']))
    assert len(found)==1, ('missing unique fixture tool',body.get('tools'))
    namespace,name=found[0]
    item={'type':'function_call','id':'fc_fixture_'+str(number),'call_id':'call_fixture_'+str(number),'name':name,'arguments':'{}','status':'completed'}
    if namespace:item['namespace']=namespace
    response=response_events(number)[-1]['response'];response['output']=[item]
    return [{'type':'response.created','response':dict(response,status='in_progress',output=[])},
        {'type':'response.output_item.added','output_index':0,'item':dict(item,arguments='',status='in_progress')},
        {'type':'response.function_call_arguments.delta','item_id':item['id'],'output_index':0,'delta':'{}'},
        {'type':'response.function_call_arguments.done','item_id':item['id'],'output_index':0,'arguments':'{}'},
        {'type':'response.output_item.done','output_index':0,'item':item},{'type':'response.completed','response':response}]

class Handler(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_GET(self):
        report.setdefault('unexpected_get',[]).append(self.path);self.send_error(404)
    def do_POST(self):
        try:
            size=int(self.headers['Content-Length']);assert 0<size<8*1024*1024
            body=json.loads(self.rfile.read(size));assert self.headers.get('Authorization')=='Bearer fixture-only'
            auxiliary='Generate a concise, single-line task title' in json.dumps(body.get('input',[]))
            with lock:
                n=len(report['model_requests'])+1
                report['model_requests'].append({'at':time.monotonic(),'case':context['case'],'auxiliary':auxiliary,'body':body})
                tool=not auxiliary and not context['tool_sent']
                if tool:context['tool_sent']=True
            events=call_events(n,body) if tool else response_events(n)
            data=''.join('data: '+json.dumps(event)+'\n\n' for event in events).encode()
            self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        except BaseException:
            report.setdefault('model_errors',[]).append(traceback.format_exc());self.send_error(500)

server=ThreadingHTTPServer(('127.0.0.1',0),Handler);server.daemon_threads=True
threading.Thread(target=server.serve_forever,daemon=True).start()
receiver=None;owned={}

def capture_owned():
    if receiver is None:return
    for process in list(receiver.owned):
        try:
            for child in [process,*process.children(recursive=True)]:
                identity=(child.pid,child.create_time())
                if identity not in owned:
                    owned[identity]=child
                    if child not in receiver.owned:receiver.owned.append(child)
        except psutil.NoSuchProcess:pass

def wait(fn,seconds=40):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        value=fn()
        if value:return value
        time.sleep(.1)
    raise TimeoutError('private fixture acceptance condition timed out')

def rpc(endpoint,request,allow_error=False):
    channel=WindowsSmokePipe(endpoint,timeout=5,read_timeout=5,write_timeout=5,max_line=4*1024*1024)
    try:
        channel.write((json.dumps(request)+'\n').encode('utf-8'))
        result=json.loads(channel.readline())
    finally:channel.close()
    if not allow_error and result.get('type')=='error':raise RuntimeError(result)
    return result

scratch=tempfile.mkdtemp(prefix='AgentDocker MCP forms ü ')
try:
    root=Path(scratch).resolve();project=root/'project';profile=root/'profile';state=root/'state';endpoint=r'\\.\pipe\agentdocker-mcp-form-'+uuid.uuid4().hex
    try:
        identity=subprocess.check_output(['whoami.exe','/user','/fo','csv','/nh'],text=True,timeout=10)
        sid=next(csv.reader(io.StringIO(identity.strip())))[1];assert sid.startswith('S-1-5-')
        subprocess.run(['icacls.exe',str(root),'/inheritance:r','/grant:r','*'+sid+':(OI)(CI)F'],check=True,capture_output=True,timeout=10)
        project.mkdir();profile.mkdir()
        subprocess.run(['git','init','-q',str(project)],check=True,timeout=5)
        mcp_log=out/'mcp-wire.jsonl'
        mcp_args=[str(evidence/mcp_driver),str(mcp_log),str(root/'trigger.json') if a.idle else a.mode]
        config='model = "fixture-model"\nmodel_provider = "fixture"\napproval_policy = "on-request"\nsandbox_mode = "danger-full-access"\ncheck_for_update_on_startup = false\n[features]\napps = false\n[analytics]\nenabled = false\n[model_providers.fixture]\nname = "Private form review fixture"\nbase_url = '+json.dumps(f'http://127.0.0.1:{server.server_port}/v1')+'\nwire_api = "responses"\nenv_key = "AGENTDOCKER_FIXTURE_KEY"\nrequest_max_retries = 0\nstream_max_retries = 0\nsupports_websockets = false\n[projects.'+json.dumps(str(project))+']\ntrust_level = "trusted"\n[mcp_servers.review_fixture]\ncommand = '+json.dumps(sys.executable)+'\nargs = '+json.dumps(mcp_args)+'\nrequired = true\ntool_timeout_sec = 90\n[mcp_servers.review_fixture.tools.'+tool_name+']\napproval_mode = "approve"\n'
        (profile/'config.toml').write_text(config,encoding='utf-8')
        report['config_sha256']=hashlib.sha256((profile/'config.toml').read_bytes()).hexdigest()
        env={k:v for k,v in os.environ.items() if k.upper() in ['PATH','SYSTEMROOT','WINDIR','USERPROFILE','TEMP','TMP','LOCALAPPDATA','APPDATA','COMSPEC','PATHEXT','PROGRAMFILES','PROGRAMFILES(X86)','LANG','LC_ALL','PYTHONUTF8']}
        env.update(CODEX_HOME=str(profile),AGENTDOCKER_HOME=str(state),AGENTDOCKER_SOCKET=str(endpoint),AGENTDOCKER_NO_AUTOSTART='1',AGENTDOCKER_NO_NOTIFICATIONS='1',AGENTDOCKER_FIXTURE_KEY='fixture-only')
        receiver=Receiver(bin,root,project,profile,out,env,report)
        receiver.socket=endpoint;receiver.env['AGENTDOCKER_SOCKET']=endpoint
        receiver.start_daemon();capture_owned()
        report['daemon_pid']=receiver.daemon.pid
        human=rpc(endpoint,{'op':'me','workdir':str(project)})['agent']['id']
        peer=rpc(endpoint,{'op':'register','spec':{'name':'fixture-peer','runtime':'fixture','workdir':str(project)}})['agent']['id']
        run=subprocess.run([str(cli),'run','--runtime','codex','--codex-input','--name','mcp-form-fixture','--workdir',str(project),'--restart','no','--',str(codex)],cwd=project,env=env,capture_output=True,text=True,encoding='utf-8',timeout=15)
        assert run.returncode==0,run.stderr
        agent=run.stdout.strip();report['agent']=agent
        ledger_path=state/'codex-input'/agent/'delivery.json'
        def inspect():
            value=rpc(endpoint,{'op':'inspect','agent':agent})['agent']
            report['last_observed_agent']=value
            if value.get('pid'):
                try:process=psutil.Process(value['pid'])
                except psutil.NoSuchProcess as error:
                    raise AssertionError('managed controller exited before acceptance; inspect last_observed_agent and retained controller log') from error
                if process not in receiver.owned:receiver.owned.append(process)
            capture_owned();return value
        def ledger():
            # Match the product's atomic snapshot contract: a Windows reader
            # must share deletion while the writer replaces the prior record.
            inspect();return read_snapshot(ledger_path) if ledger_path.exists() else {}
        wait(lambda:inspect().get('input_delivery',{}).get('reported_at'),30)
        initial=inspect();report['initial_agent']=initial;assert not initial['input_delivery']['paused'],initial['input_delivery'];report['initial_pid']=initial['pid']
        def questions():return [q for q in rpc(endpoint,{'op':'questions','agent':'user'})['questions'] if q['from']==agent]
        def send(sender,text):return rpc(endpoint,{'op':'send','from':sender,'to':agent,'kind':'chat','payload':{'text':text}})['message']
        if a.idle:
            first=send(human,'MCP_IDLE_ARM_'+uuid.uuid4().hex);report['initial_input']=first
            def armed():
                value=ledger()
                return value if value.get('attempt') is None and [v['message'] for v in value.get('completed',[])]==[first] else None
            report['initial_completed_ledger']=wait(armed,45)
            assert not report['initial_completed_ledger']['reviews'] and not questions()
        for decision in ['Submit' if a.mode=='form' else 'Accept','Decline','Cancel','cancel-route']:
            context['case']=decision
            if not a.idle:context['tool_sent']=False
            entry={'decision':decision};report['cases'].append(entry)
            nonce='MCP_'+a.mode.upper()+'_'+decision+'_'+uuid.uuid4().hex
            if a.idle:
                before_trigger=ledger();entry['before_trigger']=before_trigger
                assert before_trigger['attempt'] is None and not before_trigger['reviews']
                entry['model_requests_before']=len(report['model_requests'])
                entry['trigger_id']='fixture-elicitation-'+str(len(report['cases']))
                trigger=root/'trigger.json';staging=root/'trigger.staging'
                staging.write_text(json.dumps({'mode':a.mode,'id':entry['trigger_id']}),encoding='utf-8')
                staging.replace(trigger)
            else:first=send(human,nonce);entry['input']=first
            question=wait(lambda:next(iter(questions()),None),45);entry['question']=question
            presentation=question['presentation'];assert presentation['kind']=='mcp_'+a.mode and presentation['server']=='review_fixture',presentation
            if a.mode=='form':
                assert presentation['schema']['type']=='object' and set(presentation['schema']['properties'])==({'count'} if a.idle else {'name','count','enabled','color','tags','email','date','time','uri','optional'})
            else:
                assert presentation['url']==('https://example.com/private-fixture' if a.idle else 'https://example.com/agentdocker-fixture?case='+str(len(report['cases'])))
                assert presentation['elicitation_id']=='fixture-elicitation-'+str(len(report['cases']))
            peer_message=send(peer,'PEER_'+nonce);entry['peer_input']=peer_message
            before=ledger();assert len(before['reviews'])==1 and before['reviews'][0]['response'] is None
            if a.idle:
                assert before['attempt'] is None and before['reviews'][0]['turn'] is None
                assert before['reviews'][0]['thread']==before_trigger['thread']
            else:assert before['attempt']['message']==first and before['attempt']['acknowledged']
            callback=before['reviews'][0]['id'];entry['provider_request']=callback
            time.sleep(1)
            during=ledger();assert during['reviews'][0]['response'] is None and not during.get('steering')
            assert peer_message not in [v['message'] for v in during['completed']]
            entry['held_ledger']=during;entry['held_at']=time.monotonic()
            if a.idle:
                assert during['attempt'] is None and during['completed']==before_trigger['completed']
                assert len(report['model_requests'])==entry['model_requests_before']
                assert not any(v['direction']=='in' and v['value'].get('id')==entry['trigger_id'] and 'method' not in v['value'] for v in map(json.loads,mcp_log.read_text(encoding='utf-8').splitlines()))
            invalid=rpc(endpoint,{'op':'answer','from':'user','message':question['id'],'text':'not-a-decision'},True)
            assert invalid.get('code')=='invalid' and any(q['id']==question['id'] for q in questions())
            entry['invalid_answers']=[invalid]
            content={'name':'Ada','count':3,'enabled':False,'color':'green','tags':['a','b'],'email':'fixture@example.com','date':'2024-02-29','time':'2026-10-04T12:00:00Z','uri':'urn:example:private-fixture'}
            if a.idle:content={'count':2}
            entry['submitted_content']=content if decision=='Submit' else None
            invalid_content=dict(content,count=4) if a.mode=='form' else {'accept':True}
            invalid=rpc(endpoint,{'op':'answer','from':'user','message':question['id'],'text':json.dumps(invalid_content)},True)
            assert invalid.get('code')=='invalid' and any(q['id']==question['id'] for q in questions())
            entry['invalid_answers'].append(invalid)
            entry['decision_at']=time.monotonic()
            if decision=='cancel-route':
                answer=rpc(endpoint,{'op':'cancel_question','agent':agent,'message':question['id']})
            else:answer=rpc(endpoint,{'op':'answer','from':'user','message':question['id'],'text':json.dumps(content) if decision=='Submit' else decision})
            entry['answer']=answer
            def finished():
                value=ledger();history={v['message']:v for v in value.get('completed',[])}
                return value if first in history and peer_message in history and value.get('attempt') is None else None
            done=wait(finished,45)
            closure=next(c for c in done['closed_reviews'] if c['request']['id']==callback);entry['closed_review']=closure
            assert closure['outcome']=='resolved' and closure['acknowledged']
            expected='accept' if decision in ('Submit','Accept') else decision.lower() if decision!='cancel-route' else 'cancel'
            assert closure['request']['response']=={'id':callback,'result':{'action':expected,'content':content if decision=='Submit' else None}}
            ids=[peer_message] if a.idle else [first,peer_message]
            receipt={v['message']:v['receipt'] for v in done['completed'] if v['message'] in ids};entry['receipts']=receipt
            assert len(receipt)==len(ids)
            if not a.idle:assert receipt[first]['item']!=receipt[peer_message]['item']
            # Ordinary input resumes after review resolution; it can steer the
            # original still-active turn or start the next turn, per CODEX-INPUT.
            assert all(v['thread']==done['thread'] for v in receipt.values())
            assert not questions()
            # Direct provider launch preserves the exact managed ancestry. The
            # provider response, closure and MCP-side receipt are independent
            # witnesses; no instrumentation wrapper changes process ownership.
            assert closure['request']['thread']==done['thread']
            assert closure['request']['turn']==(None if a.idle else receipt[first]['turn'])
            assert len(done['completed'])==(1+len(report['cases']) if a.idle else len(report['cases'])*2)
            if a.idle:
                assert [r['message'] for r in done['completed']]==[report['initial_input'],*[c['peer_input'] for c in report['cases']]]
                assert len(report['model_requests'])==entry['model_requests_before']+1
            replies=[v['value'] for v in map(json.loads,mcp_log.read_text(encoding='utf-8').splitlines()) if v['direction']=='in' and 'method' not in v['value'] and str(v['value'].get('id','')).startswith('fixture-elicitation-')]
            assert replies[-1]['result']['action']==expected
            # Codex forwards URL Accept as an empty MCP object, while the
            # app-server closure above retains content:null. No fields are sent.
            wire_content=content if decision=='Submit' else {} if decision=='Accept' else None
            assert replies[-1]['result'].get('content')==wire_content
            if a.idle:
                wire=next(v for v in map(json.loads,mcp_log.read_text(encoding='utf-8').splitlines()) if v['direction']=='in' and v['value'].get('id')==entry['trigger_id'] and 'method' not in v['value'])
                assert wire['at']>=entry['decision_at'];entry['wire_reply']=wire
            entry['mcp_reply']=replies[-1];entry['result']='passed'
        assert len(replies)==4 and len({v['id'] for v in replies})==4
        report['final_ledger']=ledger();report['final_agent']=inspect()
        report['direct_provider_launch']=True
        report['fixture_tool_approval']='Only the private synthetic '+tool_name+' tool is configured approve; its decision still requires the product human review.'
        assert report['final_agent']['pid']==report['initial_pid']
        assert report['final_agent']['process_started_at']==report['initial_agent']['process_started_at']
        report['config_unchanged']=hashlib.sha256((profile/'config.toml').read_bytes()).hexdigest()==report['config_sha256']
        assert report['config_unchanged']
        assert not report.get('model_errors') and not report.get('unexpected_get')
        report['result']='passed'
    except BaseException:
        report['error']=traceback.format_exc()
    finally:
        if receiver is not None:
            try:
                for logpath in state.rglob('*.log'):
                    if logpath.is_file():
                        (out / ('retained-'+str(logpath.relative_to(state)).replace('\\','_').replace('/','_'))).write_bytes(logpath.read_bytes()[-1048576:])
                if 'ledger_path' in locals() and ledger_path.exists():
                    (out/'retained-ledger.json').write_bytes(read_shared_file(ledger_path))
            except BaseException:report['cleanup_errors'].append('retaining diagnostics: '+traceback.format_exc())
            try:capture_owned()
            except BaseException:report['cleanup_errors'].append('capturing process identities: '+traceback.format_exc())
            report['watched_processes']=[{'pid':pid,'birth':birth} for pid,birth in owned]
            try:receiver.close()
            except BaseException:report['cleanup_errors'].append(traceback.format_exc())
            report['remaining_processes']=[{'pid':pid,'birth':birth} for (pid,birth),process in owned.items() if process.is_running()]
            if report['cleanup_errors'] or report['remaining_processes']:report['result']='failed'
except BaseException:
    report['error']=traceback.format_exc();report['result']='failed'
finally:
    server.shutdown();server.server_close()
    try:remove_fixture(Path(scratch).resolve())
    except BaseException:
        report['cleanup_errors'].append(traceback.format_exc());report['result']='failed'
report['scratch_removed']=not Path(scratch).exists()
(out/'result.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps({k:v for k,v in report.items() if k not in ['model_requests','final_ledger','cases']},indent=2))
raise SystemExit(report['result']!='passed')
