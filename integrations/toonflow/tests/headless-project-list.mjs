// Standalone headless regression. Real built web UI, synthetic read-only HTTP API.
const { chromium } = await import(process.env.SEECUT_PLAYWRIGHT_MODULE || 'playwright-core');
import { createServer } from 'node:http';
import { readFile,writeFile,mkdir } from 'node:fs/promises';
import { resolve,extname } from 'node:path';
import assert from 'node:assert/strict';
if(!process.env.SEECUT_QA_WEB_ROOT)throw new Error('SEECUT_QA_WEB_ROOT must select the exact built Flow web assets');
const root=resolve(process.env.SEECUT_QA_WEB_ROOT);
const out=resolve(process.env.SEECUT_QA_OUTPUT || '.toonflow/headless-project-list');await mkdir(out,{recursive:true});
const calls=[];const projects=[['11111111-1111-4111-8111-111111111111','Synthetic Alpha'],['22222222-2222-4222-8222-222222222222','Synthetic Beta']].map(([id,name])=>({id,name,createdAt:'2026-10-20T00:00:00Z',updatedAt:'2026-10-20T00:00:00Z',sync:{state:'local'}}));
const server=createServer(async(req,res)=>{
 const path=new URL(req.url,'http://localhost').pathname;
 if(path.startsWith('/api/')||path.startsWith('/__seecut/')) {
  calls.push({method:req.method,path});res.setHeader('Content-Type','application/json');
  if(path==='/__seecut/projects')return res.end(JSON.stringify({projects}));
  if(path==='/__seecut/account')return res.end(JSON.stringify({signedIn:false}));
  return res.end(JSON.stringify({code:200,data:{},success:true}));
 }
 const target=resolve(root,'.'+(path==='/'?'/index.html':path));
 if(!target.startsWith(root+'/')){res.writeHead(403);return res.end();}
 try {const body=await readFile(target);res.setHeader('Content-Type',({'.html':'text/html','.js':'text/javascript','.css':'text/css','.json':'application/json','.svg':'image/svg+xml','.woff2':'font/woff2'})[extname(target)]??'application/octet-stream');res.end(body);}catch{res.writeHead(404);res.end();}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin=`http://127.0.0.1:${server.address().port}`;
let browser;const errors=[];const checks=[];
try {
 browser=await chromium.launch({headless:true,executablePath:process.env.SEECUT_CHROMIUM_EXECUTABLE});
 const context=await browser.newContext({viewport:{width:1200,height:800},locale:'zh-CN',serviceWorkers:'block'});
 await context.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
 await context.addInitScript(()=>{window.__SEECUT_EMBEDDED__=true;window.__SEECUT_THEME__='dark';window.ipc={postMessage(){}};});
 const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
 await page.goto(origin+'/?seecut=1#/seecut/projects');
 await page.getByRole('button',{name:'打开 Synthetic Alpha',exact:true}).waitFor();
 await page.getByRole('searchbox',{name:'搜索 Flow 项目'}).fill('Beta');
 assert.equal(await page.getByRole('button',{name:'打开 Synthetic Alpha',exact:true}).count(),0);
 assert.equal(await page.getByRole('button',{name:'打开 Synthetic Beta',exact:true}).count(),1);checks.push('page-local keyboard filtering');
 await page.getByRole('searchbox',{name:'搜索 Flow 项目'}).fill('');
 await page.getByRole('button',{name:'新建项目',exact:true}).click();
 await page.getByRole('textbox',{name:'Flow 项目名称'}).fill('Independent page input');
 await page.getByRole('button',{name:'取消',exact:true}).click();
 await page.getByRole('dialog').waitFor({state:'hidden'});checks.push('page-local click/dialog/input/cancel');
 for(const theme of ['dark','light'])for(const width of [1200,420]) {
  await page.setViewportSize({width,height:800});
  await page.evaluate(theme=>window.dispatchEvent(new CustomEvent('seecut:theme',{detail:theme})),theme);
  assert.equal(await page.locator('html').getAttribute('data-seecut-theme'),theme);
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  await page.screenshot({path:out+`/${theme}-${width}.png`,animations:'disabled'});checks.push(`${theme} ${width}px no horizontal overflow`);
 }
 assert.equal(errors.length,0,errors.join('\n'));
 assert.ok(calls.every(c=>c.method==='GET'),'No persistent fixture mutations');
 await writeFile(out+'/verification.json',JSON.stringify({checks,errors,calls,headless:true,systemInput:false,scope:'Actual bundled Flow project-list web assets; synthetic read-only API. Does not validate native Slint/Wry overlay or generation chain.'},null,2)+'\n');
 console.log(JSON.stringify({passed:checks.length,errors,headless:true}));
}finally{await browser?.close();server.close();}
