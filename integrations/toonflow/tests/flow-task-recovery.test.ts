import {test,expect,mock} from 'bun:test';
let graph:any,dispose:()=>void;
mock.module('vue',()=>({nextTick:async()=>{},onScopeDispose:(fn:()=>void)=>{dispose=fn}}));
mock.module('@vue-flow/core',()=>({useVueFlow:()=>graph}));
mock.module('@earendil-works/pi-agent-core',()=>({runAgentLoop:()=>{throw Error('unexpected model call')}}));
mock.module('@earendil-works/pi-ai',()=>({createAssistantMessageEventStream:()=>{throw Error('unexpected model call')}}));
mock.module('eventsource-parser/stream',()=>({EventSourceParserStream:class {}}));
const {useNodeAi}=await import('../../../external/toonflow/packages/nodeScaffold/src/nodeAi');
const original={clientRequestId:'original-request',mediaType:'image',outputDirectory:'assets/image',providerId:'seecut',modelId:'synthetic',prompt:'fixture',params:{},status:'running',createdAt:'now',updatedAt:'now'};
function setup(){let saves=0;const node:any={id:'image',data:{seecutGeneration:structuredClone(original),seecutGenerationHistory:[]}};graph={findNode:(id:string)=>id==='image'?node:undefined,getNodes:{value:[node]},[Symbol.for('toonflow.canvas.flush')]:async()=>{saves++}};return {node,ai:useNodeAi(),saves:()=>saves};}
test('reopened original task query updates real result, cost and immutable asset without submitting',async()=>{
 const f=setup(),before=globalThis.fetch;let reads=0,posts=0;
 const result={mediaType:'image',path:'assets/image/result.png',mimeType:'image/png',source:{assetId:'asset-one',version:{sha256:'a'.repeat(64)}}};
 globalThis.fetch=(async(url:any,options:any)=>{if(options?.method&&options.method!=='GET')posts++;const parsed=new URL(url,'http://fixture');expect(parsed.pathname).toBe('/api/ai/media/recover');expect(parsed.searchParams.get('clientRequestId')).toBe('original-request');expect(parsed.searchParams.get('directory')).toBe('project-one');reads++;return Response.json({code:200,data:{...original,status:'completed',capturedCredits:2,results:[result]}})}) as any;
 try{const outputs=await f.ai.recoverRecordedMedia('image','project-one','assets/image',undefined,true);expect(outputs).toEqual([result]);expect(f.node.data.seecutGeneration.capturedCredits).toBe(2);expect(f.node.data.seecutGeneration.status).toBe('completed');expect(f.node.data.seecutGeneration.results[0].source.assetId).toBe('asset-one');expect(f.saves()).toBe(1);expect(reads).toBe(1);expect(posts).toBe(0);}finally{globalThis.fetch=before;dispose();}
});
test('one-shot running or unknown query persists state and never resumes or generates',async()=>{
 const before=globalThis.fetch;
 try{for(const status of ['running','unknown','prepared']){const f=setup();let reads=0;globalThis.fetch=(async(url:any)=>{expect(String(url).startsWith('/api/ai/media/recover?')).toBe(true);reads++;return Response.json({code:200,data:{...original,status,dispatched:status!=='prepared'}})}) as any;expect(await f.ai.recoverRecordedMedia('image','project-one','assets/image',undefined,true)).toEqual([]);expect(f.node.data.seecutGeneration.status).toBe(status);expect(reads).toBe(1);expect(f.saves()).toBe(1);dispose();}}finally{globalThis.fetch=before;}
});
test('replaced node record and mismatched original identity cannot receive stale results',async()=>{
 const before=globalThis.fetch;
 try{for(const swapped of [false,true]){const f=setup();globalThis.fetch=(async()=>{if(swapped)f.node.data.seecutGeneration={...original,clientRequestId:'new-request'};return Response.json({code:200,data:{...original,clientRequestId:swapped?'original-request':'foreign-request',status:'running'}})}) as any;await expect(f.ai.recoverRecordedMedia('image','project-one','assets/image',undefined,true)).rejects.toThrow();expect(f.saves()).toBe(0);dispose();}}finally{globalThis.fetch=before;}
});
test('submit once, stop waiting, reopen and query completion without a second generation or capture',async()=>{
 const before=globalThis.fetch;let submissions=0,captures=0,originalId='';
 const stop=new AbortController();let f=setup();delete f.node.data.seecutGeneration;
 const output={mediaType:'image',path:'assets/image/finished.png',mimeType:'image/png',source:{assetId:'original-asset',version:{sha256:'b'.repeat(64)}}};
 globalThis.fetch=(async(url:any,options:any)=>{
  if(url==='/api/ai/media/generate'){
   submissions++;originalId=JSON.parse(options.body).clientRequestId;stop.abort();throw new DOMException('Stopped waiting','AbortError');
  }
  const parsed=new URL(url,'http://fixture');expect(parsed.pathname).toBe('/api/ai/media/recover');expect(parsed.searchParams.get('clientRequestId')).toBe(originalId);
  return Response.json({code:200,data:{...original,clientRequestId:originalId,status:'completed',capturedCredits:2,results:[output]}});
 }) as any;
 try{
  await expect(f.ai.generateImage({directory:'project-one',providerId:'seecut',modelId:'synthetic',prompt:'fixture',outputDirectory:'assets/image'},stop.signal)).rejects.toThrow();
  expect(f.node.data.seecutGeneration.status).toBe('unknown');const saved=JSON.parse(JSON.stringify(f.node.data));dispose();
  // The synthetic server completes the already submitted task once while closed.
  captures++;f=setup();f.node.data=saved;
  expect(await f.ai.recoverRecordedMedia('image','project-one','assets/image',undefined,true)).toEqual([output]);
  expect(f.node.data.seecutGeneration.clientRequestId).toBe(originalId);expect(f.node.data.seecutGeneration.capturedCredits).toBe(2);
  await f.ai.recoverRecordedMedia('image','project-one','assets/image',undefined,true);
  expect({submissions,captures}).toEqual({submissions:1,captures:1});
 }finally{globalThis.fetch=before;dispose();}
});
