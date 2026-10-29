// Fresh-browser MCP App fixture. This does not establish acceptance in a vendor host.
import http from 'node:http';
import {readFileSync,mkdirSync,writeFileSync} from 'node:fs';
import {createRequire} from 'node:module';
const require=createRequire(process.env.ZEUS_PLAYWRIGHT_PACKAGE || import.meta.url);
const {chromium}=require('@playwright/test');
const out=process.env.ZEUS_MCP_APP_EVIDENCE;
if(!out)throw Error('ZEUS_MCP_APP_EVIDENCE must name a scratch evidence directory');
mkdirSync(out,{recursive:true});
const html=readFileSync(new URL('../../ui/transfer-monitor.html',import.meta.url),'utf8');
const proxy=`<!doctype html><html><body style="margin:0"><iframe id="app" sandbox="allow-scripts" src="/app" style="width:100%;height:650px;border:0"></iframe><iframe id="sibling" sandbox="allow-scripts" src="/sibling" hidden></iframe><script>
window.seen=[];window.errors=[];window.spoofed=false;
window.send=(method,params)=>document.querySelector('#app').contentWindow.postMessage({jsonrpc:'2.0',method,params},'*');
window.result={structuredContent:{jobs:[{job_id:'job_fixture_a',untrusted_filename:'Research archive.zip',state:'downloading',bytes_done:5242880,bytes_total:10485760,rate_bps:2097152},{job_id:'job_fixture_b',untrusted_filename:'<img src="https://invalid.example/injection" onerror="alert(1)">',state:'completed',bytes_done:1024,bytes_total:1024,rate_bps:0},{job_id:'job_fixture_c',untrusted_filename:'Review requested.pdf',state:'awaiting_approval',bytes_done:0,bytes_total:2048,rate_bps:0}]}};
window.addEventListener('message',e=>{if(e.source===document.querySelector('#sibling').contentWindow&&e.data==='spoof-sent'){window.spoofed=true;return;}if(e.source!==document.querySelector('#app').contentWindow)return;const m=e.data;seen.push(m);if(m.method==='ui/initialize'){e.source.postMessage({jsonrpc:'2.0',id:m.id,result:{protocolVersion:'2026-01-26',hostInfo:{name:'Isolated fixture host',version:'1.0'},hostCapabilities:{serverTools:{}},hostContext:{theme:'dark'}}},'*')}if(m.method==='tools/call'){if(m.params.name!=='zeus_status'||m.params.arguments.limit!==50){errors.push('unexpected tool/args')}e.source.postMessage({jsonrpc:'2.0',id:m.id,result:window.result},'*')}});
window.spoof=()=>document.querySelector('#sibling').contentWindow.postMessage('spoof','*');
</script></body></html>`;
const sibling=`<!doctype html><script>addEventListener('message',()=>{parent.frames[0].postMessage({jsonrpc:'2.0',method:'ui/notifications/tool-result',params:{structuredContent:{jobs:[{job_id:'spoof',untrusted_filename:'SIBLING SPOOF',state:'completed'}]}}},'*');parent.postMessage('spoof-sent','*')})</script>`;
const server=http.createServer((req,res)=>{res.setHeader('Content-Type','text/html; charset=utf-8');res.end(req.url==='/app'?html:req.url==='/proxy'?proxy:req.url==='/sibling'?sibling:'<!doctype html><iframe id="proxy" src="/proxy" style="border:0;width:100%;height:740px"></iframe>')});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port;
const checks=[],errors=[],network=[];let browser;
function check(name,ok){checks.push({name,ok:!!ok});if(!ok)throw Error(name)}
try{
 browser=await chromium.launch({...(process.env.ZEUS_BROWSER_CHANNEL?{channel:process.env.ZEUS_BROWSER_CHANNEL}:{}),headless:true});
 const context=await browser.newContext({viewport:{width:1000,height:820}}),page=await context.newPage();page.on('pageerror',e=>errors.push(String(e)));page.on('request',r=>network.push(r.url()));
 await page.goto(`http://127.0.0.1:${port}/`);const app=page.frame({url:`http://127.0.0.1:${port}/app`}),host=page.frame({url:`http://127.0.0.1:${port}/proxy`});if(!app||!host)throw Error('fixture frames absent');
 await app.locator('#refresh').waitFor({state:'visible'});await app.waitForFunction(()=>!document.querySelector('#refresh').disabled);
 check('real browser executes CSP-pinned script and initializes',await host.evaluate(()=>seen.some(m=>m.method==='ui/notifications/initialized')));
 await app.locator('#refresh').click();await app.locator('#rows tr').nth(2).waitFor();
 check('read-only refresh passes exact bounded tool arguments',await host.evaluate(()=>seen.some(m=>m.method==='tools/call'&&m.params.name==='zeus_status'&&m.params.arguments.limit===50)&&errors.length===0));
 check('progress and summary render',await app.locator('#active').innerText()==='1'&&await app.locator('#finished').innerText()==='1'&&(await app.locator('#rate').innerText()).includes('2.0 MiB/s'));
 check('remote filename is text only',await app.locator('#rows img').count()===0&&(await app.locator('#rows').innerText()).includes('<img src='));
 await host.evaluate(()=>spoof());await host.waitForFunction(()=>window.spoofed);await page.waitForTimeout(150);check('sibling frame cannot spoof a host result',!(await app.locator('#rows').innerText()).includes('SIBLING SPOOF'));
 await host.evaluate(()=>send('ui/notifications/host-context-changed',{theme:'light',styles:{variables:{'--color-background-primary':'#fafbf9'}}}));
 check('host theme changes apply',await app.locator('html').getAttribute('data-theme')==='light');
 await page.screenshot({path:out+'/monitor-desktop.png'});await page.setViewportSize({width:360,height:820});await page.screenshot({path:out+'/monitor-narrow.png'});
 check('narrow view stays within viewport',await app.evaluate(()=>document.documentElement.scrollWidth<=document.documentElement.clientWidth));
 await host.evaluate(()=>send('ui/notifications/tool-result',{structuredContent:{jobs:[]}}));check('empty state renders',await app.locator('#rows').innerText()==='No transfers in this result.');
 await host.evaluate(()=>document.querySelector('#app').contentWindow.postMessage({jsonrpc:'2.0',id:700,method:'ping',params:{}},'*'));await page.waitForTimeout(100);check('host ping receives a response',await host.evaluate(()=>seen.some(m=>m.id===700&&m.result)));
 await host.evaluate(()=>document.querySelector('#app').contentWindow.postMessage({jsonrpc:'2.0',id:701,method:'ui/resource-teardown',params:{}},'*'));await page.waitForTimeout(100);check('teardown responds and disables requests',await app.locator('#refresh').isDisabled()&&await host.evaluate(()=>seen.some(m=>m.id===701&&m.result)));
 check('monitor makes no external network request',network.every(u=>u.startsWith(`http://127.0.0.1:${port}/`)));check('no browser page errors',errors.length===0);
 console.log(JSON.stringify({passed:checks.length,checks,pageErrors:errors,requests:network.length}));
}catch(e){errors.push(String(e));console.error(String(e));process.exitCode=1}
finally{writeFileSync(out+'/results.json',JSON.stringify({checks,errors,network},null,2));await browser?.close();await new Promise(r=>server.close(r));}
