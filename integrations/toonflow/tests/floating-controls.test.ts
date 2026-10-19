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
