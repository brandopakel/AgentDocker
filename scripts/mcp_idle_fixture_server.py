"""Private MCP server: finish its tool, then elicit only on an external trigger."""
import json,sys,threading,time,traceback
from pathlib import Path
log=Path(sys.argv[1]);trigger=Path(sys.argv[2]);lock=threading.Lock();stop=threading.Event()
def record(direction,value):
    with log.open('a',encoding='utf-8') as stream:stream.write(json.dumps({'at':time.monotonic(),'direction':direction,'value':value})+'\n')
def send(value):
    with lock:
        value=dict(jsonrpc='2.0',**value);record('out',value);print(json.dumps(value),flush=True)
def late():
    try:
        while not stop.wait(.05):
            if not trigger.exists():continue
            value=json.loads(trigger.read_text(encoding='utf-8'));trigger.unlink()
            assert value['mode'] in ['form','url']
            params={'mode':value['mode'],'message':'Private fixture: a review after the model turn has ended.'}
            if value['mode']=='form':params['requestedSchema']={'type':'object','required':['count'],'properties':{'count':{'type':'integer','minimum':1,'maximum':3}}}
            else:params.update(elicitationId=value['id'],url='https://example.com/private-fixture')
            send({'id':value['id'],'method':'elicitation/create','params':params})
    except BaseException:
        with lock:record('fixture_error',traceback.format_exc())
threading.Thread(target=late,daemon=True).start()
try:
    while line:=sys.stdin.buffer.readline(1024*1024):
        value=json.loads(line)
        with lock:record('in',value)
        request=value.get('id');method=value.get('method')
        if method=='initialize':send({'id':request,'result':{'protocolVersion':value['params']['protocolVersion'],'capabilities':{'tools':{}},'serverInfo':{'name':'private-late-review','version':'1'}}})
        elif method=='tools/list':send({'id':request,'result':{'tools':[{'name':'arm_late_review','description':'Arm the private later review fixture and immediately finish.','inputSchema':{'type':'object','properties':{},'additionalProperties':False}}]}})
        elif method=='tools/call':
            assert value['params']['name']=='arm_late_review'
            send({'id':request,'result':{'content':[{'type':'text','text':'Private later review fixture is armed; this tool is complete.'}]}})
        elif method=='ping':send({'id':request,'result':{}})
        elif method and request is not None:send({'id':request,'error':{'code':-32601,'message':'Unsupported private fixture method'}})
finally:stop.set()
