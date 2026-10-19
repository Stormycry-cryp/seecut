import {test,expect} from 'bun:test';
import {floatingControls} from '../../../external/toonflow/packages/nodeScaffold/src/floatingControls';
test('controls remain reachable after pan/zoom and in narrow viewports without moving nodes',()=>{
 for(const width of [320,640,900,1400])for(const zoom of [.25,1,2])for(const x of [-600,120,1200])for(const y of [-500,340,900]){
  const node={left:x,top:y,width:240*zoom,height:260*zoom},before=JSON.stringify(node);
  for(const side of ['top','bottom'] as const){const p=floatingControls(node,{left:0,top:0,width,height:640},side==='top'?320:660,900,side);
   expect(p.left).toBeGreaterThanOrEqual(12);expect(p.left+p.width).toBeLessThanOrEqual(width-12);expect(p.top).toBeGreaterThanOrEqual(12);expect(p.top+p.maxHeight).toBeLessThanOrEqual(576);
  }
  expect(JSON.stringify(node)).toBe(before);
 }
});

import {floatingControlPair} from '../../../external/toonflow/packages/nodeScaffold/src/floatingControls';
test('paired toolbar and prompt never collide when the node pans above the viewport',()=>{
 for(const y of [-240,-1000]){
  const pair=floatingControlPair({left:120,top:y,width:240,height:260},{left:0,top:0,width:1400,height:640},{width:320,height:44},{width:660,height:300});
  expect(pair.top!.top).toBe(12);
  expect(pair.bottom!.top).toBe(68);
  expect(pair.top!.top+pair.top!.maxHeight+12).toBeLessThanOrEqual(pair.bottom!.top);
  expect(pair.bottom!.maxHeight).toBe(300);
 }
});
test('paired controls share the viewport budget at narrow, short, zoomed and panned boundaries',()=>{
 for(const width of [160,320,640,1400])for(const height of [120,160,240,320,640])
 for(const zoom of [.25,1,2])for(const x of [-600,120,1200])for(const y of [-1000,-240,340,900])
 for(const toolbarHeight of [44,260,900]){
  const node={left:x,top:y,width:240*zoom,height:260*zoom},before=JSON.stringify(node);
  const pair=floatingControlPair(node,{left:0,top:0,width,height},{width:320,height:toolbarHeight},{width:660,height:900});
  for(const p of [pair.top!,pair.bottom!]){
   expect(p.left).toBeGreaterThanOrEqual(12);expect(p.left+p.width).toBeLessThanOrEqual(width-12);
   expect(p.top).toBeGreaterThanOrEqual(12);expect(p.maxHeight).toBeGreaterThan(0);
   expect(p.top+p.maxHeight).toBeLessThanOrEqual(height-64-12+1e-9);
  }
  expect(pair.top!.top+pair.top!.maxHeight).toBeLessThan(pair.bottom!.top);
  expect(pair.bottom!.maxHeight).toBeGreaterThan(pair.top!.maxHeight);
  expect(JSON.stringify(node)).toBe(before);
 }
});
test('a missing toolbar or prompt does not reserve space for an invisible partner',()=>{
 const anchor={left:120,top:-240,width:240,height:260},viewport={left:0,top:0,width:320,height:240};
 const bottom=floatingControlPair(anchor,viewport,null,{width:660,height:300});
 expect(bottom.top).toBeNull();expect(bottom.bottom!.maxHeight).toBe(152);
 const top=floatingControlPair(anchor,viewport,{width:320,height:260},null);
 expect(top.bottom).toBeNull();expect(top.top!.maxHeight).toBe(152);
});
