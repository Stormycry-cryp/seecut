import {test,expect,mock} from 'bun:test';
import {mkdtemp,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createRequire} from 'node:module';
let account={signedIn:true,scopeKey:'owner',epoch:1};
mock.module('../../../external/toonflow/apps/server/src/seecutNative',()=>({nativeCall:async()=>account}));
const {createSeecutFlowOperations}=await import('../../../external/toonflow/apps/server/src/seecutFlowOperations');
const require=createRequire(new URL('../../../external/toonflow/apps/server/package.json',import.meta.url));
const express=require('express');
test('durable receipts survive router recreation, reject changed intent and remain owner scoped',async()=>{
 const dir=await mkdtemp(join(tmpdir(),'seecut-flow-receipts-'));
 let server:any;
 const start=async()=>{const app=express();app.use(createSeecutFlowOperations(dir));server=await new Promise<any>(resolve=>{const s=app.listen(0,'127.0.0.1',()=>resolve(s))});return `http://127.0.0.1:${server.address().port}`};
 const close=()=>new Promise<void>((resolve,reject)=>server.close((e:any)=>e?reject(e):resolve()));
 let url=await start();
 const call={projectId:'project',canvasId:'canvas',sessionId:'opening',accountScope:'owner:1',clientId:'assistant',revision:4,operationId:'original',name:'flow_add_node',args:{type:'text'}};
 const post=async(action:string,c=call,result?:any)=>{const r=await fetch(url+'/flow/operation',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({call:c,action,result})});return {status:r.status,value:await r.json()}};
 try{
  expect((await post('get')).value).toBeNull();expect((await post('begin')).value.status).toBe('accepted');
  const finished=await post('finish',call,{status:'completed',saved:true,result:{node:{id:'created'}}});expect(finished.value.operationId).toBe('original');
  await close();url=await start();expect((await post('get')).value).toEqual(finished.value);
  expect((await post('get',{...call,args:{type:'image'}})).status).toBe(409);
  expect((await post('get',{...call,name:'flow_get_operation',sessionId:'reopened',revision:0,args:{}})).value).toEqual(finished.value);
  expect((await post('get',{...call,clientId:'another-client'})).value).toBeNull();
  account={signedIn:true,scopeKey:'other',epoch:2};expect((await post('get')).status).toBe(409);
 }finally{await close();await rm(dir,{recursive:true,force:true});}
});

test('reserved generation UUID survives server recreation and cannot be replaced by retry or finish',async()=>{
 account={signedIn:true,scopeKey:'owner',epoch:1};const dir=await mkdtemp(join(tmpdir(),'seecut-flow-generation-receipts-'));let server:any;
 const start=async()=>{const app=express();app.use(createSeecutFlowOperations(dir));server=await new Promise<any>(resolve=>{const s=app.listen(0,'127.0.0.1',()=>resolve(s))});return `http://127.0.0.1:${server.address().port}`};const close=()=>new Promise<void>((resolve,reject)=>server.close((e:any)=>e?reject(e):resolve()));let url=await start();
 const call={projectId:'project',canvasId:'canvas',sessionId:'opening',accountScope:'owner:1',clientId:'assistant',revision:4,operationId:'generation',name:'flow_execute_node',args:{nodeId:'image',name:'node:generateImage',args:{}}};
 const post=async(action:string,c:any=call,result?:any)=>{const r=await fetch(url+'/flow/operation',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({call:c,action,result})});return {status:r.status,value:await r.json()}};
 try{const reserved=(await post('begin')).value;expect(reserved.generation).toMatchObject({version:1,operationId:'generation',nodeId:'image',stage:'reserved'});await close();url=await start();expect((await post('begin')).value).toEqual(reserved);expect((await post('get',{...call,name:'flow_get_operation',sessionId:'reopened',revision:0,args:{}})).value).toEqual(reserved);
  expect((await post('finish',call,{status:'completed',saved:true,result:{status:'generating'}})).status).toBe(409);
  const finished=await post('finish',call,{status:'completed',saved:true,result:{status:'recorded',generation:{...reserved.generation,stage:'recorded'}}});expect(finished.value.generation.clientRequestId).toBe(reserved.generation.clientRequestId);expect(finished.value.generation.stage).toBe('recorded');expect((await post('begin')).value).toEqual(finished.value);
 }finally{await close();await rm(dir,{recursive:true,force:true});}
});
