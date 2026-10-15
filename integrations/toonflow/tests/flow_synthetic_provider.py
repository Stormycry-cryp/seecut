"""Synthetic provider for native UI QA. Uses real tool replies; no model network."""
import json
from uuid import uuid4
from test_assistant_contract import FakeAssistant


class SyntheticFlowAssistant(FakeAssistant):
    def __init__(self, directory):
        super().__init__()
        self.directory = directory
        self.stage = 'context'
        self.scope = {}
        self.nodes = {}
        self.polls = 0
        self.calls_in_turn = 0

    def stream(self, operation_id, prepared, cancelled):
        request = json.loads(prepared.payload)
        self.calls_in_turn += 1
        if self.calls_in_turn == 7:
            self.calls_in_turn = 0
            yield {'type':'done','message':{'id':uuid4().hex,'role':'assistant','content':[{'type':'text','text':'合成供应商本段验收已保存，请发送“继续”检查下一段。'}]},'stopReason':'stop','usage':{'input':prepared.input_token_upper_bound,'output':100}}
            return
        tools = [m for m in request['messages'] if m['role'] == 'tool']
        result = None
        if tools:
            for part in tools[-1]['content']:
                if part.get('type') == 'text':
                    try:
                        result = json.loads(part['text'])
                        break
                    except ValueError:
                        pass
        if self.stage != 'context' and (not result or result.get('status') != 'ok'):
            raise ValueError('synthetic fixture requires a successful actual tool result')
        if result:
            with (self.directory/'tool-receipts.jsonl').open('a') as output:
                output.write(json.dumps({'stage': self.stage, 'result': result}, ensure_ascii=False)+'\n')
        data = result.get('data', {}) if result else {}
        if data.get('binding'):
            self.scope = {k: data['binding'][k] for k in ('projectId','canvasId','sessionId','accountScope','revision')}
        marker = '可信宿主身份和当前状态（只含协议字段）：'
        if marker in request['system']:
            current, _ = json.JSONDecoder().raw_decode(request['system'].rsplit(marker,1)[1])
            assert current['kind']=='flow'
            self.scope = {'projectId':current['projectId'],'revision':current['revision'],**current['flow']}
        name, args = None, {}
        if self.stage == 'context':
            name, self.stage = 'context', 'workspace'
        elif self.stage == 'workspace':
            doc = next(d for d in data['documents'] if d['kind'] == 'flow')
            self.scope = {k: doc[k] for k in ('projectId','canvasId','sessionId','accountScope','revision')}
            name, self.stage = 'flow_get_workspace', 'create_text'
        elif self.stage == 'create_text':
            types = data['canvas']['availableNodeTypes']
            self.types = {key: next(t['type'] for t in types if t['label'] == label) for key, label in [('text','文本'),('image','图片生成')]}
            name, args, self.stage = 'flow_add_node', {'type':self.types['text'],'position':{'x':0,'y':0},'label':'R2 Agent text'}, 'text_tools'
        elif self.stage == 'text_tools':
            self.nodes['text'] = data['result']['node']['id']
            name, args, self.stage = 'flow_get_node_tools', {'nodeIds':[self.nodes['text']]}, 'set_text'
        elif self.stage == 'set_text':
            assert any(t['name']=='node:setText' for t in data['nodeTools'])
            name, args, self.stage = 'flow_update_node', {'nodeId':self.nodes['text'],'name':'node:setText','args':{'text':'Synthetic fixture white square'}}, 'create_image'
        elif self.stage == 'create_image':
            name, args, self.stage = 'flow_add_node', {'type':self.types['image'],'position':{'x':440,'y':0},'label':'R2 Agent image'}, 'image_config'
        elif self.stage == 'image_config':
            self.nodes['image'] = data['result']['node']['id']
            name, args, self.stage = 'flow_get_node_config', {'nodeId':self.nodes['image']}, 'set_config'
        elif self.stage == 'set_config':
            model = next(m for m in data['models'] if m['providerId']=='seecut' and 'nano-banana' in m['label'].lower() and '1K' in m['imageSizes'])
            name, args, self.stage = 'flow_update_node', {'nodeId':self.nodes['image'],'name':'node:setConfig','args':{'providerId':'seecut','modelId':model['modelId'],'size':'1K','ratio':'1:1'}}, 'ports'
        elif self.stage == 'ports':
            name, args, self.stage = 'flow_read_canvas', {'nodeIds':list(self.nodes.values()),'fields':['ports']}, 'connect'
        elif self.stage == 'connect':
            source = next(n for n in data['nodes'] if n['id']==self.nodes['text'])['ports']
            target = next(n for n in data['nodes'] if n['id']==self.nodes['image'])['ports']
            source = next(p for p in source if p['type']=='source')
            target = next(p for p in target if p['type']=='target')
            name, args, self.stage = 'flow_connect_nodes', {'connections':[{'source':self.nodes['text'],'sourceHandle':source['id'],'target':self.nodes['image'],'targetHandle':target['id']}]}, 'execute'
        elif self.stage == 'execute':
            name, args, self.stage = 'flow_execute_node', {'nodeId':self.nodes['image'],'name':'node:generateImage','args':{}}, 'task'
        elif self.stage == 'task':
            record = data.get('record')
            if record and record['status']=='completed':
                assert record['results'] and all(r.get('source',{}).get('assetId') and r['source'].get('version',{}).get('sha256') for r in record['results'])
                self.stage = 'completed'
            else:
                self.polls += 1
                assert self.polls < 12, 'bounded synthetic task polling exhausted'
                name, args = 'flow_get_task', {'nodeId':self.nodes['image']}
        message = {'id':uuid4().hex, 'role':'assistant', 'content':[]}
        if name:
            if name.startswith('flow_'):
                args = {**self.scope, 'args':args}
            message['toolCalls'] = [{'id':uuid4().hex,'name':name,'arguments':args}]
        else:
            message['content'] = [{'type':'text','text':'本机合成供应商验收：已通过正式工具创建、配置、连接并生成，结果默认入库。请重开同一项目核对持久化结果。本次未调用真实 GLM 或付费生图。'}]
        yield {'type':'done','message':message,'stopReason':'tools' if name else 'stop','usage':{'input':prepared.input_token_upper_bound,'output':min(1000,len(json.dumps(message)))}}
