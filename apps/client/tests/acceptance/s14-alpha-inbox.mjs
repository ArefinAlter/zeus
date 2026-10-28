// Native UI + real daemon intake acceptance in a private scratch profile. No approval clicks.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { createServer } from 'node:http'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'
const output=process.env.ZEUS_ALPHA_INBOX_EVIDENCE
if(!output)throw new Error('ZEUS_ALPHA_INBOX_EVIDENCE must name private scratch evidence')
mkdirSync(output,{recursive:true})
const home=resolve(join(output,`profile-${Date.now()}`)),data=Buffer.from('durable intake fixture\n'.repeat(10000)),checks=[]
const check=(name,value)=>{assert.ok(value,name);checks.push(name);console.log('PASS '+name)}
const server=createServer((req,res)=>{
 const range=/^bytes=(\d+)-(\d*)$/.exec(req.headers.range??'')
 let start=range?Number(range[1]):0,end=range&&range[2]?Number(range[2]):data.length-1;end=Math.min(end,data.length-1)
 if(start>end){res.writeHead(416,{'content-range':`bytes */${data.length}`});res.end();return}
 res.writeHead(range?206:200,{'content-length':end-start+1,'accept-ranges':'bytes',etag:'"intake-fixture"','content-type':'application/octet-stream',...(range?{'content-range':`bytes ${start}-${end}/${data.length}`}:{})})
 res.end(req.method==='HEAD'?undefined:data.subarray(start,end+1))
})
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve))
const url=`http://127.0.0.1:${server.address().port}/fixture.bin?token=fixture-secret`
let h,ui
try{
 h=await startHarness({home});await h.ready();ui=await openUi(h)
 await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/inbox`)
 await ui.page.getByLabel('Package name',{exact:true}).fill('Native intake fixture')
 await ui.page.getByRole('button',{name:'Create package',exact:true}).click()
 await ui.page.getByRole('button',{name:'Save package',exact:true}).waitFor()
 await ui.page.getByLabel('Links, one per line',{exact:true}).fill(`${url}\n${url}`)
 await ui.page.getByRole('button',{name:'Add links to package',exact:true}).click()
 await ui.page.getByRole('status').filter({hasText:'1 added · 1 exact duplicates retained once.'}).waitFor()
 check('real UI intake deduplicates exact source requests',await ui.page.locator('.inbox-entry').count()===1)
 check('public inbox rows redact source queries',!(await ui.page.locator('.inbox-entries').innerText()).includes('fixture-secret'))
 await ui.page.getByRole('button',{name:'Review next 50',exact:true}).click()
 await ui.page.getByRole('button',{name:'Start 1 reviewed downloads',exact:true}).waitFor()
 check('real UI shows concrete reviewed destination',(await ui.page.locator('.inbox-review').innerText()).includes('fixture.bin'))
 check('intake and preview alone start no downloads',(await h.get('/api/v1/jobs')).items?.length===0||(await h.get('/api/v1/jobs')).jobs?.length===0)
 await ui.page.getByRole('button',{name:'Start 1 reviewed downloads',exact:true}).click()
 await ui.page.getByRole('heading',{name:'Saved batch receipt',exact:true}).waitFor()
 await until(()=>existsSync(join(home,'downloads','fixture.bin')),{what:'reviewed download completion',timeoutMs:60000})
 check('reviewed enqueue publishes exact fixture bytes',readFileSync(join(home,'downloads','fixture.bin')).equals(data))
 let pkg=(await h.post('/native/v1/inbox/packages-list',{})).items[0]
 const before=await h.post('/native/v1/inbox/package-get',{package_id:pkg.id})
 check('one retained accepted job follows duplicate intake',before.entries.items.length===1&&!!before.entries.items[0].job_id)
 await ui.close();ui=undefined;await h.stop();h=await startHarness({home});await h.ready()
 const after=await h.post('/native/v1/inbox/package-get',{package_id:pkg.id})
 check('package and accepted job receipt survive native engine restart',after.entries.items[0].job_id===before.entries.items[0].job_id&&after.package.enqueued_entries===1)
 ui=await openUi(h);await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/inbox`)
 await ui.page.getByRole('button',{name:/Native intake fixture/}).click()
 await ui.page.getByRole('button',{name:'Archive package',exact:true}).click()
 await ui.page.getByRole('button',{name:'Unarchive package',exact:true}).waitFor()
 check('archiving package preserves completed file',readFileSync(join(home,'downloads','fixture.bin')).equals(data))
 check('native inbox UI has no page errors',ui.errors.length===0)
 await ui.page.screenshot({path:join(output,'inbox.png'),fullPage:true})
 writeFileSync(join(output,'results.json'),JSON.stringify({passed:true,checks,pageErrors:ui.errors,profile:home},null,2))
}catch(error){if(ui){writeFileSync(join(output,'failure.json'),JSON.stringify({error:String(error),body:await ui.page.locator('body').innerText(),pageErrors:ui.errors},null,2));await ui.page.screenshot({path:join(output,'failure.png'),fullPage:true})}throw error}
finally{await ui?.close();await h?.stop();await new Promise(resolve=>server.close(resolve))}
