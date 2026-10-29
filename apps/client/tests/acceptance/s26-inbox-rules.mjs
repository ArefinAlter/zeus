// Real daemon/UI with a loopback fixture; no grants, approval decisions or owner-profile access.
import assert from 'node:assert/strict'
import {existsSync,mkdirSync,readFileSync,writeFileSync} from 'node:fs'
import {join,resolve} from 'node:path'
import {createServer} from 'node:http'
import {startHarness,until} from '../harness.mjs'
import {openUi} from '../ui.mjs'
const output=process.env.ZEUS_INBOX_RULES_EVIDENCE
if(!output)throw Error('Set ZEUS_INBOX_RULES_EVIDENCE to fresh private evidence')
mkdirSync(output,{recursive:true});const home=resolve(join(output,'profile'));if(existsSync(home))throw Error('Use a fresh profile')
const data=Buffer.from('reviewed durable package fixture\n'.repeat(8000)),checks=[],check=(name,value)=>{assert.ok(value,name);checks.push(name);console.log('PASS '+name)}
const server=createServer((req,res)=>{const m=/^bytes=(\d+)-(\d*)$/.exec(req.headers.range??'');let start=m?Number(m[1]):0,end=m?.[2]?Number(m[2]):data.length-1;end=Math.min(end,data.length-1);if(start>end){res.writeHead(416,{'content-range':`bytes */${data.length}`});res.end();return}res.writeHead(m?206:200,{'content-length':end-start+1,'accept-ranges':'bytes',etag:'"package-rules-fixture"','content-type':'application/octet-stream',...(m?{'content-range':`bytes ${start}-${end}/${data.length}`}:{})});res.end(req.method==='HEAD'?undefined:data.subarray(start,end+1))})
await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin=`http://127.0.0.1:${server.address().port}`,url=origin+'/fixture.bin?token=ZEUS-CANARY-private-intake';let h,ui
try{
 h=await startHarness({home});await h.ready();ui=await openUi(h);const page=ui.page
 await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/grabber`)
 await page.getByLabel('Links or a page to grab from').fill(url);await page.getByRole('button',{name:'Add links',exact:true}).click()
 let dropped=false;await page.route('**/native/v1/inbox/package-put',async route=>{if(dropped)return route.continue();dropped=true;const response=await route.fetch();assert.equal(response.status(),200);await route.abort('failed')})
 await page.getByRole('button',{name:'Save selection to Inbox',exact:true}).click();await page.getByText('The selection could not be confirmed in Inbox.',{exact:false}).waitFor()
 const faultErrors=ui.errors.splice(0);check('only the injected lost-response network error occurred',faultErrors.every(error=>error.includes('net::ERR_FAILED')));
 const initial=(await h.post('/native/v1/inbox/packages-list',{})).items;check('lost response occurs after one durable package with one request exists',dropped&&initial.length===1&&initial[0].pending_entries===1)
 await page.unroute('**/native/v1/inbox/package-put');await page.getByRole('button',{name:'Save selection to Inbox',exact:true}).click();await page.getByRole('button',{name:'Save package',exact:true}).waitFor()
 const id=initial[0].id;check('same selection retry reopens its exact saved package',(await h.post('/native/v1/inbox/packages-list',{})).items.length===1&&new URL(page.url()).hash.includes(id))
 const allJobs=await h.get('/api/v1/jobs');check('atomic intake alone starts no downloads',(allJobs.items??allJobs.jobs??[]).length===0)
 await page.getByRole('button',{name:'Add rule',exact:true}).click();await page.getByLabel('Rule 1 name',{exact:true}).fill('archive');await page.getByLabel('Rule 1 origin',{exact:true}).fill(origin);await page.getByLabel('Rule 1 suffix',{exact:true}).fill('.bin');await page.getByLabel('Rule 1 prefix',{exact:true}).fill('saved_')
 check('unsaved package rules disable review',await page.getByRole('button',{name:'Review next 50',exact:true}).isDisabled());await page.getByRole('button',{name:'Save package',exact:true}).click();await until(()=>page.getByRole('button',{name:'Review next 50',exact:true}).isEnabled())
 await page.getByRole('button',{name:'Review next 50',exact:true}).click();await page.getByRole('button',{name:'Start 1 reviewed downloads',exact:true}).waitFor();check('native preview identifies rule and concrete safe filename',(await page.getByLabel('Reviewed download batch').innerText()).includes('saved_fixture.bin')&&(await page.getByLabel('Reviewed download batch').innerText()).includes('Rule: archive'))
 await page.getByLabel('Rule 1 prefix',{exact:true}).fill('kept_');check('editing a rule removes the prior execution control',await page.getByRole('button',{name:'Start 1 reviewed downloads',exact:true}).count()===0);await page.getByRole('button',{name:'Save package',exact:true}).click();await until(()=>page.getByRole('button',{name:'Review next 50',exact:true}).isEnabled());await page.getByRole('button',{name:'Review next 50',exact:true}).click();await page.getByRole('button',{name:'Start 1 reviewed downloads',exact:true}).waitFor()
 for(const width of [1440,960,390]){
  await page.setViewportSize({width,height:850});await page.waitForTimeout(150)
  check('Inbox stays inside the native content area at '+width,await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1))
  const overflow=await page.locator('.inbox-content input,.inbox-content textarea,.inbox-content button').evaluateAll(nodes=>nodes.filter(node=>{const r=node.getBoundingClientRect(),parent=node.closest('.inbox-content').getBoundingClientRect();return r.width>0&&(r.left<parent.left-1||r.right>parent.right+1)}).map(node=>node.getAttribute('aria-label')||node.textContent))
  check('Inbox controls remain within the panel at '+width,overflow.length===0);await page.screenshot({path:join(output,'inbox-'+width+'.png')})
 }
 await page.getByRole('button',{name:'Start 1 reviewed downloads',exact:true}).click();await page.getByRole('heading',{name:'Saved batch receipt',exact:true}).waitFor()
 const before=await h.post('/native/v1/inbox/package-get',{package_id:id,include_history:true});const destination=before.previews.find(p=>p.preview_id===before.submissions[0].preview_id).entries[0].dest_path
 await until(()=>existsSync(destination),{what:'reviewed file completion',timeoutMs:60000});check('reviewed file contains the exact fixture bytes',readFileSync(destination).equals(data)&&destination.endsWith('kept_fixture.bin'))
 await page.getByLabel('Show saved reviews and receipts').check();await page.getByText('Completed dispatch',{exact:true}).waitFor();check('visible history redacts private URL material',!(await page.getByLabel('Package history').innerText()).includes('ZEUS-CANARY'))
 check('public saved package projections redact private intake',!JSON.stringify(before).includes('ZEUS-CANARY'));check('no approvals were created',(await h.get('/api/v1/approvals')).items?.length===0)
 check('no frontend errors before restart',ui.errors.length===0);await ui.close();ui=null;await h.stop();h=await startHarness({home});await h.ready();const after=await h.post('/native/v1/inbox/package-get',{package_id:id,include_history:true});check('rules, original accepted job and history survive restart',after.package.rules[0].filename_prefix==='kept_'&&after.entries.items[0].job_id===before.entries.items[0].job_id&&after.submissions[0].batch_id===before.submissions[0].batch_id)
 ui=await openUi(h);await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/inbox?package=${id}`);await until(async()=>await ui.page.getByLabel('Rule 1 prefix',{exact:true}).inputValue()==='kept_');check('deep link restores the persisted package after restart',true);check('no frontend page errors',ui.errors.length===0)
 writeFileSync(join(output,'result.json'),JSON.stringify({ok:true,checks,approvals_made:0,page_errors:ui.errors},null,2))
}catch(error){writeFileSync(join(output,'failure.json'),JSON.stringify({error:String(error),checks,body:await ui?.page.locator('body').innerText().catch(()=>''),stderr:h?.stderr()},null,2));await ui?.page.screenshot({path:join(output,'failure.png')}).catch(()=>{});throw error}
finally{await ui?.close();await h?.stop();await new Promise(r=>server.close(r))}
