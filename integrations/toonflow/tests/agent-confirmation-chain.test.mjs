// Actual COTO runtime/approval/journal loop with an explicitly simulated MCP editor.
// This is NOT native WebView, provider generation, asset registration or UI evidence.
import test from 'node:test';
import assert from 'node:assert/strict';
import {resolve} from 'node:path';
import {existsSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const root=process.env.SEECUT_TEST_AGENT_SOURCE??resolve(import.meta.dirname,'../../../.toonflow/agent-source');
const available=existsSync(resolve(root,'test/fixtures.mjs'));
const {binding,mockEditor,setup,scriptedProvider,done}=available?await import(pathToFileURL(resolve(root,'test/fixtures.mjs'))):{};
const operations=['flow_add_node','flow_update_node','flow_add_node','flow_update_node','flow_connect_nodes','flow_execute_node','flow_get_task'];
for(const allowed of [true,false])test(`COTO approval chain with simulated editor only; allow=${allowed}`,{skip:!available},async()=>{
 const mock=mockEditor(),base=mock.transport.send.bind(mock.transport);
 const graph={revision:0,nodes:[],edges:[],writes:[],submissions:0,queries:0,simulatedAssets:[]};
 const scope=()=>({projectId:binding.projectId,canvasId:'canvas',sessionId:binding.documentSessionId,accountScope:'synthetic-account',revision:graph.revision});
 const properties={projectId:{type:'string'},canvasId:{type:'string'},sessionId:{type:'string'},accountScope:{type:'string'},revision:{type:'integer'},args:{type:'object'},operationId:{type:'string'},name:{type:'string'}};
 const reply=(id,data)=>queueMicrotask(()=>mock.transport.onmessage?.({jsonrpc:'2.0',id,result:data}));
 const response=data=>({content:[{type:'text',text:JSON.stringify(data)}]});
 mock.transport.send=async message=>{
  if(message.method==='tools/list')return reply(message.id,{tools:['context',...new Set(operations)].map(name=>({name,description:name,inputSchema:name==='context'?{type:'object',properties:{}}:{type:'object',properties,required:['projectId','canvasId','sessionId','accountScope','revision','args'],additionalProperties:false}}))});
  if(message.method!=='tools/call')return base(message);
  const {name,arguments:a}=message.params;
  if(name==='context')return reply(message.id,response({status:'ok',data:{appInstanceId:mock.state.instanceId,activeWorkspace:'flow',documents:[{kind:'flow',...scope(),documentSessionId:binding.documentSessionId,selectionRevision:0,selectedObjectIds:[],contextRevision:graph.revision,busy:null}]}}));
  assert.equal(a.revision,graph.revision);
  if(name==='flow_get_task'){
   graph.queries++;
   return reply(message.id,response({status:'ok',data:{record:{status:'completed',results:graph.simulatedAssets}}}));
  }
  assert.ok(a.operationId);assert.ok(!graph.writes.some(w=>w.operationId===a.operationId));graph.writes.push(a);
  let result={};
  if(name==='flow_add_node'){const node={id:'node-'+(graph.nodes.length+1),...a.args};graph.nodes.push(node);result={node};}
  if(name==='flow_update_node'){const node=graph.nodes.find(n=>n.id===a.args.nodeId);assert.ok(node);Object.assign(node,a.args.args);result={node};}
  if(name==='flow_connect_nodes'){graph.edges.push(...a.args.connections);result={connected:graph.edges.length};}
  if(name==='flow_execute_node'){
   graph.submissions++;graph.simulatedAssets.push({source:{assetId:'SIMULATED-ASSET',version:{sha256:'a'.repeat(64)}}});result={status:'generating'};
  }
  graph.revision++;
  return reply(message.id,response({status:'ok',data:{operationId:a.operationId,status:'completed',saved:true,binding:scope(),result}}));
 };
 const provider=scriptedProvider(async function*(_request,index){
  if(!allowed&&index>0){yield done('取消后不执行后续步骤');return;}
  const name=operations[index];if(!name){yield done('模拟编辑端链完成');return;}
  let args;
  if(index===0)args={type:'textNode',position:{x:0,y:0}};
  if(index===1)args={nodeId:graph.nodes[0].id,name:'node:setText',args:{text:'synthetic prompt'}};
  if(index===2)args={type:'imageGenerationNode',position:{x:440,y:0}};
  if(index===3)args={nodeId:graph.nodes[1].id,name:'node:setConfig',args:{providerId:'seecut',modelId:'synthetic'}};
  if(index===4)args={connections:[{source:graph.nodes[0].id,sourceHandle:'text',target:graph.nodes[1].id,targetHandle:'prompt'}]};
  if(index===5)args={nodeId:graph.nodes[1].id,name:'node:generateImage',args:{}};
  if(index===6)args={nodeId:graph.nodes[1].id};
  yield done('',[{id:'step-'+index,name,arguments:{...scope(),args}}]);
 });
 const env=await setup(provider,mock);
 try{
  await env.runtime.openSession(binding);
  await env.runtime.send({sessionId:binding.sessionId,inputId:'synthetic-chain',mode:'follow_up',content:[{type:'text',text:'模拟创建、设置、连接和生成'}]});
  let cursor=0,approvals=0,final;
  while(true){
   const frame=await env.wait(f=>env.events.indexOf(f)>=cursor&&(f.type==='result'||f.event?.type==='approval.required'));
   cursor=env.events.indexOf(frame)+1;
   if(frame.type==='result'){final=frame;break;}
   approvals++;
   // No write may happen merely because the summary/card was published.
   assert.equal(graph.writes.length,allowed?approvals-1:0);
   await env.runtime.approve({sessionId:binding.sessionId,approvalId:frame.event.data.approvalId,allowed});
  }
  assert.equal(graph.writes.length,allowed?6:0);
  assert.equal(approvals,allowed?6:1);
  assert.equal(graph.submissions,allowed?1:0);assert.equal(graph.queries,allowed?1:0);
  assert.equal(graph.simulatedAssets.length,allowed?1:0);
  assert.equal(final.toolState.outcomeUnknown,false);
  assert.equal(env.runtime.get(binding.sessionId).snapshot().pendingInputs.length,0);
  if(allowed){assert.equal(final.result.status,'completed');assert.equal(graph.nodes.length,2);assert.equal(graph.edges.length,1);}
 }finally{await env.dispose();}
 assert.equal(mock.state.closed,true);
});
