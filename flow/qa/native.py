#!/usr/bin/env python3
"""Native Slint probe backed by the Rust Flow engine over local stdin/stdout.
Only synthetic fixtures. No accounts, HTTP, model calls, or user project paths.
"""
import argparse
import atexit
import json
import subprocess
import uuid
from pathlib import Path
import slint

ROOT = Path(__file__).resolve().parents[2]
NAMES = {"prompt":"提示词", "asset":"参考素材", "image_generate":"图像生成", "select":"采用结果", "video_generate":"视频生成", "deliver":"交付"}
STATES = {"ready":"待运行", "succeeded":"模拟结果 · 已记录", "stale":"输入已变更", "Intent":"待核实", "Unknown":"待核实", "CancelRequested":"已请求停止", "Cancelled":"已停止", "Failed":"失败"}
PARAMS = {"prompt":{"text":"新的提示词"}, "asset":{"asset_id":"synthetic:product","content_version":"1","media_kind":"Image"}, "image_generate":{"model":"deterministic-v1","count":2}, "select":{"media_kind":"Image"}, "video_generate":{"model":"deterministic-v1","count":1}, "deliver":{"media_kind":"Video","target":"synthetic-assets"}}
class Probe:
    def __init__(self, folder, dark=False, width=1280, height=800):
        self.folder = folder
        self.process = subprocess.Popen([str(ROOT / "flow/core/target/debug/seecut-flow-core"), "serve", str(folder)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        atexit.register(self.close)
        self.module = slint.load_file(Path(__file__).with_suffix('.slint'), style='fluent')
        self.ui = self.module.FlowProbe()
        self.ui.Theme.dark = dark
        self.ui.view_width = width; self.ui.view_height = height
        self.pending_picker = None
        self.ui.fixture_image = slint.Image.load_from_path(str(ROOT / 'flow/qa/synthetic.svg'))
        self.ui.assets_requested = self.assets
        self.ui.shared_generation_requested = self.generation
        self.ui.reference_selected = self.select
        self.ui.fixture_chosen = self.fixture
        self.snapshot = {}
        self.drafts = {}
        self.ui.message = "合成练习 · 结果由本地模拟器产生"
        self.ui.selected = self.select
        self.ui.action = self.action
        self.ui.move_node = self.move
        self.ui.viewport_settled = self.viewport
        self.ui.viewport_preview = self.edges
        self.ui.output_port = self.output_port
        self.ui.input_port = self.input_port
        self.ui.prompt_commit = self.prompt
        self.ui.draft_changed = self.draft
        self.ui.choose = self.choose
        self.node_model = slint.ListModel([])
        self.ui.nodes = self.node_model
        self.fetch()
        vp = self.snapshot['graph']['viewport']
        if vp != [0.0, 0.0, 1.0]: self.ui.pan_x, self.ui.pan_y, self.ui.zoom = vp
        self.render()
    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try: self.process.wait(timeout=3)
            except subprocess.TimeoutExpired: self.process.terminate()
    def rpc(self, obj):
        self.process.stdin.write(json.dumps(obj, ensure_ascii=False) + '\n')
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line: raise RuntimeError("Rust Flow process exited")
        result = json.loads(line)
        if not result['ok']: raise RuntimeError(result['error'])
        return result['value']
    def fetch(self):
        self.snapshot = self.rpc({'op':'snapshot'})
    def command(self, command):
        return self.send({'op':'command','document':self.snapshot['document'],'graph':self.snapshot['graph']['id'],'revision':self.snapshot['graph']['revision'],'operation':str(uuid.uuid4()),'command':command})
    def send(self, request):
        try:
            value = self.rpc(request)
            self.snapshot = value.get('snapshot', value)
            outcome = value.get('outcome','')
            self.ui.message = "结果记录已保存 · 真实生成未接入"
            self.ui.error = False
            if 'Paused' in outcome: self.ui.message = "已暂停。请选择当前批次的候选，再继续运行。"
            self.render()
            return True
        except (RuntimeError, KeyError) as error:
            self.ui.error = True
            self.ui.message = self.friendly(str(error))
            self.fetch(); self.render()
            return False
    @staticmethod
    def friendly(error):
        if 'choose one' in error: return "请先采用当前批次中的一个结果。"
        if 'upstream' in error or 'missing prompt' in error: return "上游结果尚未就绪，或输入已变化。可使用「运行到这里」。"
        if 'incompatible' in error: return "端口类型不兼容，请选择相同媒体类型的输入。"
        if 'cardinality' in error: return "这个输入已连接。请先断开现有连线。"
        if 'lost response' in error: return "结果待核实。使用「核实结果」恢复，系统不会重复提交。"
        if 'rejection' in error: return "模拟任务失败，可重试；未消耗虚拟点数。"
        if 'revision conflict' in error: return "图已更新，请重新查看后再操作。"
        if 'no edit history' in error: return "没有可撤销或重做的编辑。"
        return error
    @staticmethod
    def title(node):
        if node['title'] != node['kind']: return node['title']
        if node['kind']=='select': return '采用视频' if node['params'].get('media_kind')=='Video' else '采用图片'
        return NAMES.get(node['kind'],'未支持的节点')
    def draft(self,text):
        self.drafts[self.ui.selected_id]=text
        self.ui.save_label='未保存文本'
    def select(self, node):
        previous = self.ui.selected_id
        if previous and self.ui.selected_kind == 'prompt': self.drafts[previous] = self.ui.prompt_text
        self.ui.selected_id = node
        self.render()
    def render(self):
        graph = self.snapshot['graph']; selected = self.ui.selected_id
        rows = []
        for key, node in graph['nodes'].items():
            output = self.snapshot['outputs'].get(key)
            summary = node['params'].get('text', '') if node['kind'] == 'prompt' else '连接输入后运行'
            image_output = output and (output.get('Media',{}).get('kind') == 'Image' or any(a['kind']=='Image' for a in output.get('Candidates',{}).get('assets',[])))
            has_preview = node['kind'] == 'asset' or image_output
            if output and not has_preview and node['kind'] != 'prompt': summary = '模拟视频清单已记录 · 真实视频未接入'
            if node['kind'] == 'select' and not output: summary = '等待人工选择'
            if node['kind'] == 'deliver' and not output: summary = '保存至合成资产集'
            inputs = {'image_generate':['prompt','references'], 'video_generate':['prompt','references'], 'select':['candidates'], 'deliver':['media']}.get(node['kind'],[])
            preview = slint.Image.load_from_path(str(ROOT / 'flow/qa/synthetic.svg')) if has_preview else slint.Image()
            rows.append(self.module.FlowNodeView(id=key,title=self.title(node),kind=NAMES.get(node['kind'],'未支持的节点'),summary=summary,state=STATES.get(self.snapshot['states'][key],self.snapshot['states'][key]),x=node['position'][0],y=node['position'][1],preview=preview,has_preview=bool(has_preview),inputs=slint.ListModel(inputs)))
        if self.node_model.row_count() == len(rows):
            for index, row in enumerate(rows): self.node_model.set_row_data(index,row)
        else:
            self.node_model = slint.ListModel(rows); self.ui.nodes = self.node_model
        self.edges()
        self.ui.candidates = slint.ListModel([])
        self.ui.references = slint.ListModel([])
        self.ui.generation_parameters = slint.ListModel([])
        if selected not in graph['nodes']: self.ui.selected_id = ''; return
        node = graph['nodes'][selected]
        self.ui.selected_title = self.title(node)
        self.ui.selected_kind = node['kind']
        self.ui.selected_status = STATES.get(self.snapshot['states'][selected],self.snapshot['states'][selected])
        if node['kind'] == 'prompt':
            self.ui.prompt_text = self.drafts.get(selected,node['params']['text'])
            self.ui.save_label = '未保存文本' if self.ui.prompt_text != node['params']['text'] else '已保存'
        if node['kind'] in ('image_generate','video_generate'):
            incoming = sorted((e for e in graph['edges'] if e['to']==selected), key=lambda e:e['order'])
            prompt_source = next((e['from'] for e in incoming if e['input']=='prompt'),None)
            self.ui.generation_prompt = graph['nodes'][prompt_source]['params'].get('text','') if prompt_source else ''
            self.ui.references = slint.ListModel([self.module.FlowAttachment(id=e['from'],label=self.title(graph['nodes'][e['from']]),preview=self.ui.fixture_image) for e in incoming if e['input']=='references'])
            self.ui.generation_parameters = slint.ListModel([
                self.module.FlowParameter(id='capability',label='SeeCut 生成'),
                self.module.FlowParameter(id='count',label=str(node['params'].get('count',1))+' 份结果')])
        attempts = [a for a in self.snapshot['attempts'] if a['graph_id'] == graph['id'] and a['node_id'] == selected]
        status = attempts[-1]['status'] if attempts else ''
        self.ui.has_attempt = status in ('Unknown','Intent','CancelRequested')
        self.ui.can_cancel = status in ('Unknown','Intent')
        self.ui.can_retry = status in ('Failed','Cancelled')
        if node['kind'] == 'select':
            incoming = next((e for e in graph['edges'] if e['to'] == selected),None)
            output = self.snapshot['outputs'].get(incoming['from'],{}) if incoming else {}
            assets = output.get('Candidates',{}).get('assets',[])
            self.ui.candidates = slint.ListModel([self.module.FlowCandidate(id=a['id'],label=f"采用候选 {i+1}",selected=node['params'].get('asset_id') == a['id']) for i,a in enumerate(assets)])
        self.ui.points_label = str(self.snapshot['virtual_spent']) + ' 虚拟点'
    def edges(self,x=None,y=None,zoom=None):
        if not self.snapshot: return
        graph = self.snapshot['graph']; z = self.ui.zoom; px = self.ui.pan_x; py = self.ui.pan_y
        if x is not None: px,py,z=x,y,zoom
        edges = []
        for edge in graph['edges']:
            a = graph['nodes'][edge['from']]['position']; b = graph['nodes'][edge['to']]['position']
            x1=(a[0]+228)*z+px; y1=(a[1]+100)*z+py
            index=1 if edge['input']=='references' else 0
            x2=b[0]*z+px; y2=(b[1]+82+index*36)*z+py
            bend=max(40,abs(x2-x1)*0.5)
            path=f'M {x1} {y1} C {x1+bend} {y1} {x2-bend} {y2} {x2} {y2}'
            edges.append(self.module.FlowEdgeView(path=path,selected=self.ui.selected_id in (edge['from'],edge['to'])))
        self.ui.edges = slint.ListModel(edges)
    def move(self,node,x,y,commit):
        self.snapshot['graph']['nodes'][node]['position']=[x,y]
        if commit: self.command({'type':'move','positions':{node:[x,y]}})
        else: self.render()
    def viewport(self,x,y,z):
        self.command({'type':'viewport','viewport':[x,y,z]})
    def output_port(self,node):
        self.ui.connecting_from = node
        self.ui.message = '选择目标输入端口进行连接。'
    def input_port(self,node,port):
        source = self.ui.connecting_from
        if not source:
            matches=[e for e in self.snapshot['graph']['edges'] if e['to']==node and e['input']==port]
            if matches: self.command({'type':'disconnect','edge':matches[-1]})
            return
        orders=[e['order'] for e in self.snapshot['graph']['edges'] if e['to']==node and e['input']==port]
        if self.command({'type':'connect','edge':{'from':source,'output':'out','to':node,'input':port,'order':max(orders,default=-1)+1}}): self.ui.connecting_from=''
    def prompt(self,text):
        node=self.ui.selected_id
        self.drafts[node]=text
        if self.command({'type':'parameters','node':node,'params':{'text':text}}):
            self.drafts.pop(node,None); self.ui.save_label='已保存'
    def choose(self,asset):
        self.send({'op':'choose','node':self.ui.selected_id,'asset':asset})
    def assets(self,node,local):
        self.ui.error = False
        if local:
            self.ui.message = '本地上传交由 SeeCut 资产库导入；合成验证不读取真实文件。'
            return
        try:
            self.pending_picker = self.rpc({'op':'asset_picker','node':node})
            self.ui.fixture_picker = True
        except RuntimeError as error:
            self.ui.error=True; self.ui.message=self.friendly(str(error))
    def fixture(self,asset_id):
        if self.pending_picker is None: return
        asset={'asset':{'id':asset_id,'version':'fixture-v1','kind':'Image','simulated':True},'name':'合成资产库参考','available':True}
        if self.send({'op':'accept_asset','selection':self.pending_picker,'asset':asset}):
            self.pending_picker=None; self.ui.fixture_picker=False
            self.ui.message='合成资产库素材已连接 · 来源和内容版本已保存'
    def generation(self,node):
        # Production callback opens SeeCut's existing generation surface with Context.
        # This isolated fixture must not clone its account/quote/submission implementation.
        try:
            self.rpc({'op':'generation_draft','node':node})
            self.ui.message='已准备共享生成输入 · SeeCut 页面接线待主线集成'
            self.ui.error=False
        except RuntimeError as error:
            self.ui.message='SeeCut 生成设置由主线接入；先运行上游即可校验交接输入。'
            self.ui.error=False
    def action(self,name):
        node=self.ui.selected_id
        if name in ('run_node','run_to','retry','reject','lost_response'):
            self.send({'op':'run_to' if name=='run_to' else 'run_node','node':node,'retry':name=='retry','fault':name if name in ('reject','lost_response') else None})
        elif name in ('cancel','reconcile'):
            attempt=next((a for a in reversed(self.snapshot['attempts']) if a['node_id']==node and a['graph_id']==self.snapshot['graph']['id']),None)
            if attempt: self.send({'op':name,'attempt':attempt['id']})
        elif name in ('undo','redo'): self.send({'op':name})
        elif name == 'arrange': self.command({'type':'arrange'})
        elif name == 'duplicate': self.command({'type':'duplicate','nodes':[node],'incoming':False})
        elif name.startswith('add_'):
            kind=name[4:]; video_select=kind=="select_video"; kind="select" if video_select else kind; uid=str(uuid.uuid4())
            params=PARAMS.get(kind,PARAMS['prompt']).copy()
            if video_select: params["media_kind"]="Video"
            self.command({'type':'add','node':{'id':uid,'kind':kind,'version':1,'title':NAMES[kind],'params':params,'position':[(self.ui.view_width/2-self.ui.pan_x)/self.ui.zoom,80]}})
            self.select(uid)
        elif name=='fit':
            positions=[n['position'] for n in self.snapshot['graph']['nodes'].values()]
            if positions:
                left=min(p[0] for p in positions); right=max(p[0] for p in positions)+228
                top=min(p[1] for p in positions); bottom=max(p[1] for p in positions)+190
                self.ui.zoom=min(1,(self.ui.view_width-100)/(right-left),(self.ui.view_height-360)/(bottom-top))
                self.ui.pan_x=50-left*self.ui.zoom; self.ui.pan_y=80-top*self.ui.zoom
                self.viewport(self.ui.pan_x,self.ui.pan_y,self.ui.zoom)
    def run(self): self.ui.run()
if __name__ == '__main__':
    parser=argparse.ArgumentParser(); parser.add_argument('folder',type=Path); parser.add_argument('--dark',action='store_true'); parser.add_argument('--width',type=int,default=1280); parser.add_argument('--height',type=int,default=800); args=parser.parse_args()
    Probe(args.folder,args.dark,args.width,args.height).run()
