import {test,expect} from 'bun:test';
import {createRequire} from 'node:module';
import {createSeecutPrompts} from '../../../external/toonflow/apps/server/src/seecutPrompts';
const express=createRequire(new URL('../../../external/toonflow/apps/server/package.json',import.meta.url))('express');
test('prompt bridge binds native account and fences a changed login epoch',async()=>{
 let epoch=1;let signedIn=true;let changed=false;let limited=false;let upstream=0;
 const call=async(method:string,params:any)=>{
  if(method==='account.status')return {signedIn,scopeKey:'one',epoch};
  expect(params.expectedScopeKey).toBe('one');
  if(method==='prompts.context')return {capabilities:{recommend:true,rewrite:true},templates:[{id:'one',name:'棚拍',prompt:'棚拍'}]};
  upstream++;if(limited)throw {code:'PROMPT_RATE_LIMITED',details:{retry_after:45}};
  if(changed)epoch++;
  return {recommendations:[{id:'one',label:'棚拍'}]};
 };
 const app=express();app.use(createSeecutPrompts(call as any));
 const server=await new Promise<any>(resolve=>{const s=app.listen(0,'127.0.0.1',()=>resolve(s))});const base=`http://127.0.0.1:${server.address().port}`;
 const post=(scope='one:1')=>fetch(base+'/prompts/recommend',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({accountScope:scope,requestId:'original-request-01',input:{prompt:'studio'}})});
 try{
  const context=await fetch(base+'/prompts/context').then(r=>r.json());expect(context.accountScope).toBe('one:1');
  expect((await post('other:1')).status).toBe(409);expect(upstream).toBe(0);
  expect((await post()).status).toBe(200);changed=true;expect((await post()).status).toBe(409);
  changed=false;limited=true;const response=await post('one:2');expect(response.status).toBe(429);expect((await response.json()).retryAfter).toBe(45);
  signedIn=false;expect((await post('one:2')).status).toBe(401);
 }finally{await new Promise<void>((resolve,reject)=>server.close((error:any)=>error?reject(error):resolve()));}
});
