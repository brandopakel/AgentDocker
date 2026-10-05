"""Private stdio MCP fixture: one explicit form or URL review per tool call."""
import json, sys, time
from pathlib import Path
log = Path(sys.argv[1])
mode = sys.argv[2] if len(sys.argv) > 2 else 'form'
assert mode in ('form', 'url')
tool = 'review_with_' + mode
number = 0

def record(direction, value):
    with log.open('a',encoding='utf-8') as writer:
        writer.write(json.dumps({'at':time.monotonic(),'direction':direction,'value':value})+'\n')

def send(value):
    value = dict(jsonrpc='2.0', **value)
    record('out', value)
    print(json.dumps(value), flush=True)

def incoming():
    line = sys.stdin.buffer.readline(1024 * 1024)
    if not line: raise EOFError()
    value = json.loads(line); record('in',value); return value

def handle(value):
    global number
    method = value.get('method'); request = value.get('id')
    if method == 'initialize':
        send({'id':request,'result':{'protocolVersion':value['params']['protocolVersion'],
            'capabilities':{'tools':{}},'serverInfo':{'name':'private-review-fixture','version':'1'}}})
    elif method == 'tools/list':
        send({'id':request,'result':{'tools':[{'name':tool,'description':'Exercise the private '+mode+' review fixture once.',
            'inputSchema':{'type':'object','properties':{},'additionalProperties':False}}]}})
    elif method == 'tools/call':
        assert value['params']['name']==tool
        number += 1; elicitation = 'fixture-elicitation-'+str(number)
        params={'mode':'form',
            'message':'Private fixture: review these nonsecret preferences before submitting.',
            'requestedSchema':{'type':'object','required':['name','count','enabled','color','tags','email','date','time','uri'],
                'properties':{
                    'name':{'type':'string','minLength':2,'maxLength':40,'default':'Fixture Name'},
                    'count':{'type':'integer','minimum':1,'maximum':3,'default':2},
                    'enabled':{'type':'boolean','default':False},
                    'color':{'type':'string','enum':['blue','green'],'enumNames':['Blue','Green'],'default':'blue'},
                    'tags':{'type':'array','minItems':1,'maxItems':2,'items':{'anyOf':[{'const':'a','title':'Alpha'},{'const':'b','title':'Beta'}]}},
                    'email':{'type':'string','format':'email'}, 'date':{'type':'string','format':'date'},
                    'time':{'type':'string','format':'date-time'}, 'uri':{'type':'string','format':'uri'},
                    'optional':{'type':'string'}
                }}}
        if mode == 'url':
            params={'mode':'url','elicitationId':elicitation,
                    'message':'Private fixture: review this example destination. Do not open the website.',
                    'url':'https://example.com/agentdocker-fixture?case='+str(number)}
        send({'id':elicitation,'method':'elicitation/create','params':params})
        while True:
            reply = incoming()
            if reply.get('id') == elicitation and 'method' not in reply: break
            handle(reply)
        send({'id':request,'result':{'content':[{'type':'text','text':json.dumps({'fixture_elicitation':elicitation,'reply':reply})}]}})
    elif method == 'ping': send({'id':request,'result':{}})
    elif request is not None: send({'id':request,'error':{'code':-32601,'message':'Fixture does not implement '+str(method)}})

try:
    while True: handle(incoming())
except EOFError: pass
