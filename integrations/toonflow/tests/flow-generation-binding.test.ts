import {test,expect,mock} from 'bun:test';
import {mkdtemp,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createRequire} from 'node:module';
let graph:any,dispose=()=>{};
mock.module('vue',()=>({getCurrentScope:()=>({}),nextTick:async()=>{},onScopeDispose:(fn:()=>void)=>{dispose=fn}}));
mock.module('@vue-flow/core',()=>({useNodeId:()=>'image',useVueFlow:()=>graph}));
mock.module('@earendil-works/pi-agent-core',()=>({runAgentLoop:()=>{throw Error('unexpected model call')}}));
mock.module('@earendil-works/pi-ai',()=>({createAssistantMessageEventStream:()=>{throw Error('unexpected model call')}}));
mock.module('eventsource-parser/stream',()=>({EventSourceParserStream:class {}}));
mock.module('../../../external/toonflow/apps/server/src/seecutNative',()=>({nativeCall:async()=>({signedIn:true,scopeKey:'owner',epoch:1})}));
const {useNodeAi}=await import('../../../external/toonflow/packages/nodeScaffold/src/nodeAi');
const {createGenerationHandoff,waitForGenerationPreparation}=await import('../../../external/toonflow/packages/nodeScaffold/src/nodeGeneration');
const {createFlowGate}=await import('../../../external/toonflow/apps/web/src/lib/seecutFlowGate');
const {createSeecutFlowOperations}=await import('../../../external/toonflow/apps/server/src/seecutFlowOperations');
const require=createRequire(new URL('../../../external/toonflow/apps/server/package.json',import.meta.url));
const express=require('express');
const initial={projectId:'project',canvasId:'canvas',sessionId:'opening',accountScope:'owner:1',revision:4};
const call=(operationId='original')=>({...initial,clientId:'assistant',operationId,name:'flow_execute_node',args:{nodeId:'image',name:'node:generateImage',args:{}}});
const output={mediaType:'image',path:'assets/image/result.png',mimeType:'image/png',source:{assetId:'original-asset',version:{sha256:'a'.repeat(64)}}};
function deferred<T>(){let resolve!:(v:T)=>void,reject!:(e:unknown)=>void;const promise=new Promise<T>((a,b)=>{resolve=a;reject=b});return {promise,resolve,reject};}
async function fixture(){
 const dir=await mkdtemp(join(tmpdir(),'seecut-generation-binding-'));const app=express();app.use(createSeecutFlowOperations(dir));const server:any=await new Promise(resolve=>{const s=app.listen(0,'127.0.0.1',()=>resolve(s))});const base=`http://127.0.0.1:${server.address().port}`;
 const fetchOriginal=globalThis.fetch;const node:any={id:'image',data:{}};let saves=0,scope=initial.accountScope,binding={...initial};let save=async()=>{};
 graph={findNode:(id:string)=>id==='image'?node:undefined,getNodes:{value:[node]},[Symbol.for('toonflow.canvas.flush')]:async()=>{await save();saves++}};const ai=useNodeAi();let posts:any[]=[];let response:(body:any)=>Promise<Response>=async()=>Response.json({code:200,data:[output]});
 globalThis.fetch=(async(url:any,options:any)=>{
  if(url==='/api/ai/media/generate'){const body=JSON.parse(options.body);posts.push(body);return response(body);}
  return fetchOriginal(url,options);
 }) as any;
 const receipt=async(c:any,action:string,result?:any)=>{const r=await fetchOriginal(base+'/flow/operation',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({call:c,action,result})});const value:any=await r.json();if(!r.ok)throw Error(value.error);return value};
 let preparation=Promise.resolve({directory:'project-one'}),work:Promise<unknown>=Promise.resolve(),lastExecution:any;
 const gate=createFlowGate({binding:()=>binding,account:async expected=>{if(scope!==expected)throw Error('FLOW_ACCOUNT_CHANGED')},receipt,workspace:async()=>binding,flush:async()=>{},execute:async(command,signal,_call,execution)=>{
  if(command.args.name==='node:getConfig')return {config:{providerId:'seecut'}};
  lastExecution=execution;
  if(command.args.name==='node:getRecordedGeneration'){
   const ref=execution?.generation;if(!ref)throw Error('expected bound query');
   await ai.recoverRecordedMedia('image','project-one','assets/image',signal,true,ref.clientRequestId,execution);
   return {generation:{...ref,stage:'recorded'},record:ai.getMediaGeneration('assets/image',ref.clientRequestId)};
  }
  const handoff=createGenerationHandoff(execution,'image','image',signal);
  work=waitForGenerationPreparation(preparation,signal).then(({directory})=>ai.generateImage({directory,providerId:'seecut',modelId:'synthetic',prompt:'fixture',outputDirectory:'assets/image'},signal,handoff.execution)).catch(e=>{handoff.fail(e);return e});
  return handoff.acknowledgement;
 }});
 return {node,ai,gate,receipt,posts,saves:()=>saves,work:()=>work,execution:()=>lastExecution,setPreparation:(p:Promise<{directory:string}>)=>preparation=p,setSave:(fn:()=>Promise<void>)=>save=fn,setResponse:(fn:typeof response)=>response=fn,setScope:(v:string)=>scope=v,setBinding:(v:any)=>binding={...binding,...v},async close(){globalThis.fetch=fetchOriginal;dispose();await new Promise<void>((resolve,reject)=>server.close((e:any)=>e?reject(e):resolve()));await rm(dir,{recursive:true,force:true});}};
}
async function until(check:()=>Promise<any>){for(let n=0;n<100;n++){const v=await check();if(v)return v;await new Promise(r=>setTimeout(r,1));}throw Error('fixture did not reach boundary');}

test('server reserves original UUID before workspace read; record save precedes ack; replay never resubmits',async()=>{
 const f=await fixture();const prep=deferred<{directory:string}>(),save=deferred<void>();f.setPreparation(prep.promise);f.setSave(()=>save.promise);
 try{
  let returned=false;const running=f.gate(call(),new AbortController().signal).then(v=>{returned=true;return v});
  const accepted=await until(()=>f.receipt({...call(),name:'flow_get_operation'},'get'));expect(accepted.generation.stage).toBe('reserved');expect(f.posts).toHaveLength(0);
  const concurrent=await f.receipt(call(),'begin');expect(concurrent.generation.clientRequestId).toBe(accepted.generation.clientRequestId);
  prep.resolve({directory:'project-one'});await until(async()=>f.node.data.seecutGeneration);expect(returned).toBe(false);expect(f.posts).toHaveLength(0);
  save.resolve();const result=await running;expect(result.status).toBe('completed');expect(result.generation.stage).toBe('recorded');expect(result.result.generation.clientRequestId).toBe(accepted.generation.clientRequestId);
  await f.work();expect(f.posts).toHaveLength(1);expect(f.posts[0]).toMatchObject({clientRequestId:accepted.generation.clientRequestId,graphId:'canvas',nodeId:'image'});expect(f.node.data.seecutGeneration.flowOperation).toEqual(f.execution().generation);
  expect(await f.gate(call(),new AbortController().signal)).toEqual(result);expect(f.posts).toHaveLength(1);
 }finally{save.resolve();prep.resolve({directory:'project-one'});await f.work();await f.close();}
});

test('cancelled preparation and save failure are durable no-submit results; late workspace cannot POST',async()=>{
 for(const scenario of ['cancel','save']){const f=await fixture();const stop=new AbortController(),prep=deferred<{directory:string}>();
  try{if(scenario==='cancel')f.setPreparation(prep.promise);else f.setSave(async()=>{throw Error('disk unavailable')});const running=f.gate(call(),stop.signal);await until(()=>f.receipt({...call(),name:'flow_get_operation'},'get'));if(scenario==='cancel')stop.abort();const result=await running;expect(result.status).toBe('notExecuted');expect(result.generation.stage).toBe('notSubmitted');prep.resolve({directory:'project-one'});await f.work();expect(f.posts).toHaveLength(0);expect(await f.gate(call(),new AbortController().signal)).toEqual(result);expect(f.posts).toHaveLength(0);
  }finally{prep.resolve({directory:'project-one'});await f.work();await f.close();}
 }
});

test('late preparation under replaced account/project/node cannot write or submit another workspace',async()=>{
 for(const change of ['scope','project','node']){const f=await fixture(),prep=deferred<{directory:string}>();f.setPreparation(prep.promise);
  try{const running=f.gate(call(),new AbortController().signal);await until(()=>f.receipt({...call(),name:'flow_get_operation'},'get'));if(change==='scope')f.setScope('other:2');if(change==='project')f.setBinding({projectId:'other'});if(change==='node')graph.findNode=()=>undefined;prep.resolve({directory:'project-one'});expect((await running).status).toBe('notExecuted');await f.work();expect(f.posts).toHaveLength(0);expect(f.node.data.seecutGeneration).toBeUndefined();}
  finally{prep.resolve({directory:'project-one'});await f.work();await f.close();}
 }
});

test('unknown submission reopens with the same UUID and historical query ignores a newer manual job',async()=>{
 const f=await fixture();const before=globalThis.fetch;let reads=0;try{
  f.setResponse(async()=>{throw Error('lost POST response')});const executed=await f.gate(call(),new AbortController().signal);await f.work();const ref=executed.generation;expect(f.node.data.seecutGeneration.status).toBe('unknown');expect(f.posts).toHaveLength(1);
  // Persist/reopen records, then the user starts a different job. Original ref stays in history.
  f.node.data=JSON.parse(JSON.stringify(f.node.data));f.node.data.seecutGeneration={...f.node.data.seecutGeneration,clientRequestId:crypto.randomUUID(),status:'running',flowOperation:undefined};const manual=f.node.data.seecutGeneration;
  f.setBinding({sessionId:'reopened'});globalThis.fetch=(async(url:any,options:any)=>{if(String(url).startsWith('/api/ai/media/recover?')){reads++;expect(new URL(url,'http://fixture').searchParams.get('clientRequestId')).toBe(ref.clientRequestId);return Response.json({code:200,data:{...f.ai.getMediaGeneration('assets/image',ref.clientRequestId),status:'completed',capturedCredits:2,results:[output]}});}return before(url,options)}) as any;
  const result:any=await f.gate({...call(),sessionId:'reopened',name:'flow_get_task',args:{nodeId:'image'}},new AbortController().signal);
  expect(result.generation.clientRequestId).toBe(ref.clientRequestId);expect(result.generation.sessionId).toBe('opening');expect(result.record.status).toBe('completed');expect(result.record.results[0].source.assetId).toBe('original-asset');expect(f.node.data.seecutGeneration).toBe(manual);expect(manual.status).toBe('running');expect(reads).toBe(1);expect(f.posts).toHaveLength(1);
 }finally{await f.close();}
});

test('model cannot inject UUID; a foreign acknowledgement cannot complete reserved operation',async()=>{
 const f=await fixture();try{
  for(const args of [{...call().args,clientRequestId:crypto.randomUUID()},{...call().args,args:{clientRequestId:crypto.randomUUID()}}]){expect((await f.gate({...call(),args},new AbortController().signal)).status).toBe('notExecuted');expect(await f.receipt({...call(),name:'flow_get_operation'},'get')).toBeNull();}
  await expect(f.gate({...call(),name:'flow_get_task',args:{nodeId:'image',clientRequestId:crypto.randomUUID()}},new AbortController().signal)).rejects.toThrow('FLOW_GENERATION_QUERY_INVALID');
  const reserved=await f.receipt(call(),'begin');await expect(f.receipt(call(),'finish',{status:'completed',saved:true,result:{generation:{...reserved.generation,stage:'recorded',clientRequestId:crypto.randomUUID()}}})).rejects.toThrow('FLOW_GENERATION_CONFLICT');expect((await f.receipt({...call(),name:'flow_get_operation'},'get')).status).toBe('accepted');expect(f.posts).toHaveLength(0);
 }finally{await f.close();}
});

test('a timed-out caller retains the reserved UUID; an accepted replay never prepares a new job',async()=>{
 const f=await fixture(),prep=deferred<{directory:string}>();f.setPreparation(prep.promise);try{
  const original=f.gate(call(),new AbortController().signal);const accepted=await until(()=>f.receipt({...call(),name:'flow_get_operation'},'get'));
  // Native RPC may stop waiting while WebView preparation is still pending.
  // A reopened gate must preserve the accepted identity and cannot dispatch.
  let writes=0;const reopened=createFlowGate({binding:()=>initial,account:async()=>{},receipt:f.receipt,workspace:async()=>initial,execute:async()=>{writes++;throw Error('unexpected redispatch')},flush:async()=>{}});
  expect(await reopened(call(),new AbortController().signal)).toEqual(accepted);expect(writes).toBe(0);expect(f.posts).toHaveLength(0);
  prep.resolve({directory:'project-one'});const recorded=await original;await f.work();expect(recorded.generation.clientRequestId).toBe(accepted.generation.clientRequestId);expect(f.posts).toHaveLength(1);
 }finally{prep.resolve({directory:'project-one'});await f.work();await f.close();}
});

test('bound historical query rejects forged record scope and does not save after a node replacement',async()=>{
 const f=await fixture();const before=globalThis.fetch;try{
  const executed=await f.gate(call(),new AbortController().signal);await f.work();const ref=executed.generation;
  f.node.data.seecutGeneration.flowOperation={...ref,projectId:'foreign'};
  await expect(f.gate({...call(),name:'flow_get_task',args:{nodeId:'image'}},new AbortController().signal)).rejects.toThrow('原任务与冻结操作不匹配');
  f.node.data.seecutGeneration.flowOperation={...f.execution().generation};const saves=f.saves();const previous=f.node.data;
  globalThis.fetch=(async(url:any,options:any)=>{if(String(url).startsWith('/api/ai/media/recover?')){f.node.data={seecutGeneration:{...previous.seecutGeneration,clientRequestId:crypto.randomUUID(),status:'running'}};return Response.json({code:200,data:{...previous.seecutGeneration,status:'completed',results:[output]}});}return before(url,options)}) as any;
  await expect(f.gate({...call(),name:'flow_get_task',args:{nodeId:'image'}},new AbortController().signal)).rejects.toThrow('原任务所属节点已变化');expect(f.saves()).toBe(saves);expect(f.node.data.seecutGeneration.status).toBe('running');expect(f.posts).toHaveLength(1);
 }finally{await f.close();}
});


test('real node tool registry forwards trusted context separately from strict model arguments',async()=>{
 const {nodeTools,useNodeToolsContext,z}=await import('../../../external/toonflow/packages/nodeScaffold/src/nodeTools');
 graph={findNode:(id:string)=>id==='image'?{id,data:{}}:undefined};
 const ref={version:1 as const,...initial,operationId:'original',nodeId:'image',clientRequestId:crypto.randomUUID(),mediaType:'image' as const};
 const execution={generation:ref,assertCurrent:async()=>{}};let received:any;
 const unregister=nodeTools.register({name:'generateImage',description:'synthetic generation preparation',parameters:z.strictObject({}),execute:async(args,context)=>{expect(args).toEqual({});received=context.execution;const handoff=createGenerationHandoff(context.execution,'image','image',context.signal);await handoff.execution!.onRecorded!({clientRequestId:ref.clientRequestId,mediaType:'image',outputDirectory:'assets/image'});return handoff.acknowledgement;}});
 try{const tools=useNodeToolsContext()();expect((await tools.call({nodeId:'image',name:'node:generateImage',args:{}},undefined,execution) as any).generation).toMatchObject({clientRequestId:ref.clientRequestId,stage:'recorded'});expect(received).toBe(execution);await expect(tools.call({nodeId:'image',name:'node:generateImage',args:{generation:ref}})).rejects.toThrow();}
 finally{unregister();}
});
