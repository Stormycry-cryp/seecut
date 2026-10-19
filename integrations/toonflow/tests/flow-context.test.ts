import {test,expect,mock} from 'bun:test';
let publish:()=>void;
let state = {canvasReady:true,canvasId:'canvas',flowRevision:15,flowSelectionRevision:2,selectedNodeIds:['image']};
mock.module('vue',()=>({watch:(_source:unknown,callback:()=>void)=>{publish=callback;return ()=>{};}}));
mock.module('../../../external/toonflow/apps/web/src/lib/mcpControl.ts',()=>({getWorkspaceControl:()=>({getState:()=>state})}));
mock.module('../../../external/toonflow/apps/web/src/lib/seecutWorkspaceBridge.ts',()=>({currentSeecutProjectId:{value:'project'},seecutOpeningProject:{value:false}}));
mock.module('../../../external/toonflow/apps/web/src/lib/seecutEmbedded.ts',()=>({isSeecutEmbedded:true}));
const {installSeecutFlowControl}=await import('../../../external/toonflow/apps/web/src/lib/seecutFlowControl');

test('native context transports a distinct selection epoch through task refresh and scope changes',async()=>{
 const priorWindow=globalThis.window,priorFetch=globalThis.fetch;
 const messages:any[]=[];
 globalThis.window={ipc:{postMessage:(value:string)=>messages.push(JSON.parse(value))}} as any;
 globalThis.fetch=(async()=>Response.json({accountScope:'account:1'})) as any;
 const stop=installSeecutFlowControl();
 try {
  await new Promise(resolve=>setTimeout(resolve,0));
  const current=()=>messages.at(-1).context;
  expect(current()).toMatchObject({revision:15,selectionRevision:2,selectedNodeIds:['image']});
  const originalSession=current().sessionId;
  state.flowRevision++;publish();
  expect(current()).toMatchObject({revision:16,selectionRevision:2,sessionId:originalSession});
  state.flowSelectionRevision+=2;publish();
  expect(current()).toMatchObject({selectionRevision:4,selectedNodeIds:['image'],sessionId:originalSession});
  state.canvasId='other';publish();
  expect(current().sessionId).not.toBe(originalSession);
  expect(current().canvasId).toBe('other');
  state.canvasReady=false;publish();expect(current()).toBeNull();
 } finally {stop();globalThis.window=priorWindow;globalThis.fetch=priorFetch;}
});
