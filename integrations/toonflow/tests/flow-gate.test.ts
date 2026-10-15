import {test,expect} from 'bun:test';
import {createFlowGate,flowCommand,type FlowBinding,type FlowCall} from '../../../external/toonflow/apps/web/src/lib/seecutFlowGate';
const initial: FlowBinding={projectId:'project',canvasId:'a',sessionId:'opening',accountScope:'account:1',revision:4};
const request=():FlowCall=>({...initial,name:'flow_add_node',args:{type:'text',position:{x:0,y:0}},operationId:'operation-1'});
function fixture(){
 let binding={...initial};let scope=initial.accountScope;let edits=0;let saves=0;
 const receipts=new Map<string,any>();
 const gate=createFlowGate({binding:()=>binding,account:async expected=>{if(scope!==expected)throw Error('FLOW_ACCOUNT_CHANGED')},
 receipt:async(c,a,r)=>{const old=receipts.get(c.operationId!);if(a==='get')return old;if(a==='begin'){const result=old??{status:'accepted'};receipts.set(c.operationId!,result);return result;}receipts.set(c.operationId!,r);return r;},
 workspace:async()=>binding,execute:async()=>{edits++;binding.revision++;return {node:{id:'real-node'}}},flush:async()=>{saves++}});
 return {call:(c=request())=>gate(c,new AbortController().signal),setBinding:(v:Partial<FlowBinding>)=>Object.assign(binding,v),account:(v:string)=>scope=v,counts:()=>({edits,saves}),receipts};
}
test('actual edit/save receipt is replayed once with original stale revision',async()=>{const f=fixture();const a=await f.call();expect(a.saved).toBe(true);expect(await f.call()).toEqual(a);expect(f.counts()).toEqual({edits:1,saves:1});});
test('stale revision does not dispatch or consume operation',async()=>{const f=fixture();f.setBinding({revision:5});expect(await f.call()).toMatchObject({status:'notExecuted',operationId:'operation-1',error:'Error: FLOW_STALE_REVISION'});expect(f.counts().edits).toBe(0);expect(f.receipts.size).toBe(0);expect((await f.call({...request(),revision:5,operationId:'new-explicit-operation'})).status).toBe('completed');expect(f.counts().edits).toBe(1)});
test('switching canvas blocks original call',async()=>{const f=fixture();f.setBinding({canvasId:'b',sessionId:'new'});expect(await f.call()).toMatchObject({status:'notExecuted',error:'Error: FLOW_BINDING_CHANGED'});expect(f.counts().edits).toBe(0)});
test('account change blocks before mutation',async()=>{const f=fixture();f.account('other:2');expect(await f.call()).toMatchObject({status:'notExecuted',error:'Error: FLOW_ACCOUNT_CHANGED'});expect(f.counts().edits).toBe(0)});
test('accepted but unknown original operation never repeats',async()=>{const f=fixture();f.receipts.set('operation-1',{status:'accepted'});expect((await f.call()).status).toBe('accepted');expect(f.counts().edits).toBe(0)});
test('configuration cannot invoke generation or arbitrary node functions',()=>{expect(()=>flowCommand({...request(),name:'flow_update_node',args:{name:'node:generateImage'}})).toThrow('FLOW_TOOL_UNAVAILABLE');expect(()=>flowCommand({...request(),name:'flow_execute_node',args:{name:'node:executeShell'}})).toThrow('FLOW_TOOL_UNAVAILABLE')});
test('generation requires the actual configured managed provider before receipt allocation',async()=>{
 let dispatched=0,allocated=0;
 const gate=createFlowGate({binding:()=>initial,account:async()=>{},receipt:async(_c,a)=>{if(a==='begin')allocated++;return null},workspace:async()=>initial,execute:async c=>{if(c.args.name==='node:getConfig')return {config:{providerId:'external'}};dispatched++},flush:async()=>{}});
 expect(await gate({...request(),name:'flow_execute_node',args:{nodeId:'image',name:'node:generateImage',args:{}}},new AbortController().signal)).toMatchObject({status:'notExecuted',error:'Error: FLOW_MANAGED_PROVIDER_REQUIRED'});
 expect(dispatched).toBe(0);expect(allocated).toBe(0);
});
test('save failure leaves an original receipt and replay never repeats mutation',async()=>{
 let receipt:any,edits=0;
 const gate=createFlowGate({binding:()=>initial,account:async()=>{},receipt:async(_c,a,r)=>{if(a==='get')return receipt;if(a==='begin')return receipt={status:'accepted'};return receipt=r},workspace:async()=>initial,execute:async()=>{edits++;},flush:async()=>{throw Error('save failed')}});
 await expect(gate(request(),new AbortController().signal)).rejects.toThrow('save failed');
 expect((await gate(request(),new AbortController().signal)).status).toBe('outcomeUnknown');expect(edits).toBe(1);
});
test('abort after begin but before execution persists a definitive refusal',async()=>{
 const controller=new AbortController();let receipt:any,edits=0;
 const gate=createFlowGate({binding:()=>initial,account:async()=>{},workspace:async()=>initial,execute:async()=>{edits++},flush:async()=>{},receipt:async(_c,a,r)=>{if(a==='get')return receipt;if(a==='begin'){controller.abort();return receipt={status:'accepted'}}return receipt=r}});
 expect((await gate(request(),controller.signal)).status).toBe('notExecuted');expect(receipt.status).toBe('notExecuted');expect(edits).toBe(0);
});
test('original-task read saves refreshed node output and never allocates a write operation',async()=>{
 let reads=0,saves=0,receipts=0;
 const gate=createFlowGate({binding:()=>initial,account:async()=>{},workspace:async()=>initial,receipt:async()=>{receipts++},execute:async c=>{expect(c.args.name).toBe('node:getRecordedGeneration');reads++;return {record:{clientRequestId:'original',status:'completed',capturedCredits:2}}},flush:async()=>{saves++}});
 const result:any=await gate({...request(),name:'flow_get_task',args:{nodeId:'image'}},new AbortController().signal);expect(result.record.status).toBe('completed');expect({reads,saves,receipts}).toEqual({reads:1,saves:1,receipts:0});
});
