"""Local QA-only continuation after confirmed executed:false connection cancellation."""
import json, runpy
from uuid import uuid4
import flow_synthetic_provider as module
from test_assistant_contract import FakeAssistant
class Resume(FakeAssistant):
    def __init__(self, directory):
        super().__init__(); self.directory=directory; self.stage=0; self.polls=0
    def stream(self, operation_id, prepared, cancelled):
        request=json.loads(prepared.payload); result=None
        for m in reversed(request['messages']):
            if m['role']=='tool':
                for p in m['content']:
                    if p.get('type')=='text':
                        try: result=json.loads(p['text']); break
                        except ValueError: pass
                break
        if self.stage and (not result or result.get('status')!='ok'): raise ValueError('Expected actual successful native tool reply')
        if result:
            with (self.directory/'r2-resume-receipts.jsonl').open('a') as f: f.write(json.dumps({'stage':self.stage,'result':result},ensure_ascii=False)+'\n')
        marker='可信宿主身份和当前状态（只含协议字段）：'
        current,_=json.JSONDecoder().raw_decode(request['system'].rsplit(marker,1)[1]); assert current['kind']=='flow'
        scope={'projectId':current['projectId'],'revision':current['revision'],**current['flow']}
        assert scope['projectId']=='85d47627-eabf-43a1-9b8f-05f7536a7a10'
        text='92795d45-4af7-4e84-8660-070e6f3a90e9'; image='b80951d2-70d2-433d-a6c4-1c029b14d2c0'
        name=None; args={}; data=(result or {}).get('data',{})
        if self.stage==0: name='flow_read_canvas'; args={'nodeIds':[text,image],'fields':['ports']}
        elif self.stage==1:
            nodes=data['nodes']; assert {n['id'] for n in nodes}=={text,image}
            source=next(p for n in nodes if n['id']==text for p in n['ports'] if p['type']=='source')
            target=next(p for n in nodes if n['id']==image for p in n['ports'] if p['type']=='target')
            name='flow_connect_nodes'; args={'connections':[{'source':text,'sourceHandle':source['id'],'target':image,'targetHandle':target['id']}]}
        elif self.stage==2:
            assert data['status']=='completed' and data['saved']; name='flow_execute_node'; args={'nodeId':image,'name':'node:generateImage','args':{}}
        elif self.stage>=3:
            record=data.get('record')
            if not record or record['status']!='completed':
                self.polls+=1; assert self.polls<8; name='flow_get_task'; args={'nodeId':image}
            else:
                assert record['results'] and all(r.get('source',{}).get('assetId') and r['source'].get('version',{}).get('sha256') for r in record['results'])
        self.stage+=1
        message={'id':uuid4().hex,'role':'assistant','content':[]}
        if name: message['toolCalls']=[{'id':uuid4().hex,'name':name,'arguments':{**scope,'args':args}}]
        else: message['content']=[{'type':'text','text':'合成 R2 续接完成：已连接原节点，执行一次并核对完成结果及资产版本。请保存重开检查。本次未调用真实 GLM 或供应商。'}]
        yield {'type':'done','message':message,'stopReason':'tools' if name else 'stop','usage':{'input':prepared.input_token_upper_bound,'output':min(1000,len(json.dumps(message)))}}
module.SyntheticFlowAssistant=Resume
runpy.run_path('integrations/toonflow/tests/prompt-ui-fixture.py',run_name='__main__')
