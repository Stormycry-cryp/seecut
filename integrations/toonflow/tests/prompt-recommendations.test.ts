import {expect,test} from 'bun:test';
import {PromptRecommendations,RecommendationGate} from '../../../external/toonflow/packages/nodeShared/src/promptRecommendations';
const input=(prompt:string)=>({prompt,images:[]});
function fixture(api:any,gate=new RecommendationGate()) {let time=0;let latest:any;const applied:string[]=[];const c=new PromptRecommendations(api,s=>latest=s,p=>applied.push(p),gate,()=>time);return {c,gate,applied,get state(){return latest},at:(t:number)=>time=t};}
test('debounce and shared start interval retain latest input',async()=>{
 const calls:any[]=[];const f=fixture(async(_o:any,i:any)=>{calls.push(i);return {recommendations:[{id:'one',label:'棚拍'}]}});
 f.c.update(input('a'),'account:canvas');f.at(999);await f.c.tick();expect(calls).toHaveLength(0);
 f.at(1000);await f.c.tick();expect(calls).toHaveLength(1);
 f.c.update(input('b'),'account:canvas');f.at(2000);f.c.update(input('latest'),'account:canvas');f.at(10999);await f.c.tick();expect(calls).toHaveLength(1);
 f.at(11000);await f.c.tick();expect(calls[1].prompt).toBe('latest');
});
test('concurrent editors share interval and no parallel calls',async()=>{
 const gate=new RecommendationGate();let resolve:any;let count=0;const a=fixture(()=>new Promise(r=>{count++;resolve=r}),gate);const b=fixture(async()=>{count++;return {recommendations:[]}},gate);
 a.c.update(input('a'),'A');b.c.update(input('b'),'A');a.at(1000);b.at(1000);const p=a.c.tick();await b.c.tick();expect(count).toBe(1);resolve({recommendations:[]});await p;b.at(11000);await b.c.tick();expect(count).toBe(2);
});
test('old account or draft reply cannot repopulate recommendations',async()=>{
 let resolve:any;const f=fixture(()=>new Promise(r=>resolve=r));f.c.update(input('first'),'one');f.at(1000);const p=f.c.tick();f.c.update(input('next'),'two');resolve({recommendations:[{id:'old',label:'old'}]});await p;expect(f.state.cards).toEqual([]);
});
test('rewrite changes only prompt, and does not recursively recommend',async()=>{
 let calls=0;const f=fixture(async(o:string)=>{calls++;return o==='rewrite'?{prompt:'短改写'}:{recommendations:[{id:'one',label:'棚拍'}]}});
 f.c.update({prompt:'原文',images:['ref']},'one');f.at(1000);await f.c.tick();f.c.update({prompt:'最新需求',images:['ref']},'one');await f.c.rewrite('one');expect(f.state.status).toBe('为你推荐');expect(f.applied).toEqual(['短改写']);expect(f.c.input.images).toEqual(['ref']);f.c.update({prompt:'短改写',images:['ref']},'one');f.at(20000);await f.c.tick();expect(calls).toBe(2);
});
test('failed rewrite retains original and can be explicitly retried',async()=>{
 let count=0;const f=fixture(async(o:string)=>{if(o==='recommend')return {recommendations:[{id:'one',label:'棚拍'}]};count++;throw Error('offline')});f.c.update(input('original'),'one');f.at(1000);await f.c.tick();await f.c.rewrite('one');expect(f.applied).toEqual([]);expect(f.c.input.prompt).toBe('original');expect(f.state.rewriting).toBe(false);await f.c.rewrite('one');expect(count).toBe(2);
});
test('429 retains draft and observes supplier retry after',async()=>{
 let count=0;const f=fixture(async()=>{count++;if(count===1)throw {status:429,retryAfter:60};return {recommendations:[]}});f.c.update(input('same'),'one');f.at(1000);await f.c.tick();f.at(60000);await f.c.tick();expect(count).toBe(1);f.at(61000);await f.c.tick();expect(count).toBe(2);
});
test('switch or unmount fences pending rewrite',async()=>{
 let resolve:any;const f=fixture(async(o:string)=>o==='recommend'?{recommendations:[{id:'one',label:'棚拍'}]}:new Promise(r=>resolve=r));f.c.update(input('a'),'one');f.at(1000);await f.c.tick();const p=f.c.rewrite('one');f.c.update(input('b'),'two');resolve({prompt:'old'});await p;expect(f.applied).toEqual([]);f.c.dispose();
});

function liveFixture(rewrite:(input:any)=>Promise<any>,kind='image') {
 let time=0,live={input:{prompt:'before',images:['ref-one']},context:`account:canvas:${kind}`};
 const calls:any[]=[],applied:any[]=[];
 const c=new PromptRecommendations(async(operation,input)=>{
  calls.push({operation,input:structuredClone(input)});
  return operation==='recommend'?{recommendations:[{id:'one',label:'棚拍'}]}:rewrite(input);
 },()=>{},(prompt,operation)=>{live.input={...live.input,prompt};applied.push({prompt,operation});},new RecommendationGate(),()=>time,()=>live);
 return {c,calls,applied,get live(){return live},set live(value){live=value},at:(value:number)=>time=value,ready:async()=>{c.sync();time=1000;await c.tick();}};
}
for(const kind of ['image','video']) {
 test(`${kind}: click snapshots current draft and images without waiting for polling`,async()=>{
  const f=liveFixture(async input=>({prompt:'rewrite '+input.prompt}),kind);await f.ready();
  f.live.input={prompt:'typed immediately before click',images:['ref-one','ref-two']};
  await f.c.rewrite('one');
  expect(f.calls[1]).toEqual({operation:'rewrite',input:{prompt:'typed immediately before click',images:['ref-one','ref-two']}});
  expect(f.live.input.images).toEqual(['ref-one','ref-two']);expect(f.c.state.rewriting).toBe(false);
  f.c.undoRewrite();expect(f.live.input.prompt).toBe('typed immediately before click');expect(f.live.input.images).toEqual(['ref-one','ref-two']);
  expect(f.applied.map(v=>v.operation)).toEqual(['rewrite','undo']);expect(f.calls.map(v=>v.operation)).toEqual(['recommend','rewrite']);
 });
 for(const change of ['draft','references','context','closed']) test(`${kind}: late rewrite cannot overwrite ${change} before next poll`,async()=>{
  let resolve:any;const f=liveFixture(()=>new Promise(r=>resolve=r),kind);await f.ready();
  const pending=f.c.rewrite('one');
  if(change==='draft')f.live.input.prompt='new draft';
  if(change==='references')f.live.input.images=['replacement'];
  if(change==='context')f.live.context='another account or canvas';
  if(change==='closed')f.live.context='';
  resolve({prompt:'stale'});await pending;
  expect(f.applied).toEqual([]);expect(f.c.state.rewriting).toBe(false);expect(f.c.state.canUndo).toBe(false);
 });
}
test('in-flight rewrite cannot be entered twice; failed response preserves full draft',async()=>{
 let reject:any;const f=liveFixture(()=>new Promise((_r,j)=>reject=j));await f.ready();
 const first=f.c.rewrite('one');await f.c.rewrite('one');expect(f.calls).toHaveLength(2);
 reject(Error('offline'));await first;expect(f.live.input).toEqual({prompt:'before',images:['ref-one']});expect(f.c.state.rewriting).toBe(false);expect(f.c.state.canUndo).toBe(false);
});
test('synchronous edit notification fences edit-away-and-back and scope-away-and-back',async()=>{
 for(const change of ['draft','context']) {
  let resolve:any;const f=liveFixture(()=>new Promise(r=>resolve=r));await f.ready();const pending=f.c.rewrite('one');
  const original=structuredClone(f.live);
  if(change==='draft')f.live.input.prompt='intermediate';else f.live.context='other';f.c.sync();
  f.live=original;f.c.sync();resolve({prompt:'stale'});await pending;expect(f.applied).toEqual([]);
 }
});
test('undo is scoped to unchanged rewrite and never restores over later edits or references',async()=>{
 for(const change of ['draft','references','context']) {
  const f=liveFixture(async()=>({prompt:'rewritten'}));await f.ready();await f.c.rewrite('one');expect(f.c.state.canUndo).toBe(true);
  if(change==='draft')f.live.input.prompt='user edit';if(change==='references')f.live.input.images=['new ref'];if(change==='context')f.live.context='new account';
  const latest=structuredClone(f.live);f.c.undoRewrite();expect(f.live).toEqual(latest);expect(f.applied).toHaveLength(1);expect(f.c.state.canUndo).toBe(false);
 }
});
