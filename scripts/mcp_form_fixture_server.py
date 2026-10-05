"""Private stdio MCP fixture: one explicit form elicitation per tool call."""
import json, sys, time
from pathlib import Path
log = Path(sys.argv[1])
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
            'capabilities':{'tools':{}},'serverInfo':{'name':'private-form-review-fixture','version':'1'}}})
    elif method == 'tools/list':
        send({'id':request,'result':{'tools':[{'name':'review_with_form','description':'Exercise the private form review fixture once.',
            'inputSchema':{'type':'object','properties':{},'additionalProperties':False}}]}})
    elif method == 'tools/call':
        assert value['params']['name']=='review_with_form'
        number += 1; elicitation = 'fixture-elicitation-'+str(number)
        send({'id':elicitation,'method':'elicitation/create','params':{'mode':'form',
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
                }}}})
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
