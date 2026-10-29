// Fresh-profile native Team acceptance. No live IdP, existing account or approval is used.
import assert from 'node:assert/strict'
import { createHash, createPrivateKey, createPublicKey } from 'node:crypto'
import { createServer } from 'node:http'
import { spawn } from 'node:child_process'
import { existsSync, mkdirSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, BIN } from '../harness.mjs'
import { openUi } from '../ui.mjs'
import { Publisher } from '../../../../services/team-alpha/src/signatures.mjs'
if (!process.env.ZEUS_TEAM_EVIDENCE) throw new Error('Set ZEUS_TEAM_EVIDENCE to a new task-owned directory')
const output=resolve(process.env.ZEUS_TEAM_EVIDENCE),home=join(output,'profile'),team=join(home,'zeus','state','team')
if(existsSync(home))throw new Error('Use a fresh profile')
mkdirSync(team,{recursive:true})
const checks=[],check=(name,value)=>{assert.ok(value,name);checks.push(name);console.log('PASS '+name)}
const privateKey=createPrivateKey({key:Buffer.concat([Buffer.from('302e020100300506032b657004220420','hex'),Buffer.alloc(32,7)]),format:'der',type:'pkcs8'})
const origin='https://team.example.com',tenant='fixture',keyId='fixture',jwk=createPublicKey(privateKey).export({format:'jwk'})
const publisher=new Publisher({issuer:origin,activeKey:keyId,keys:{[keyId]:privateKey.export({format:'pem',type:'pkcs8'}).toString()}})
const pin={origin,tenant_id:tenant,keys:{[keyId]:Buffer.from(jwk.x,'base64url').toString('hex')}}
const features=downloads=>({downloads,cloud_delivery:false,workflows:true,metadata_sync:true,provenance:true})
writeFileSync(join(team,'pin.json'),JSON.stringify(pin))
writeFileSync(join(team,'policy.json'),JSON.stringify(publisher.policy(tenant,2,features(false),Date.now())))
let requests=0
const fixture=createServer((req,res)=>{requests++;const bytes=Buffer.from('Team policy fixture');res.writeHead(200,{'content-type':'application/octet-stream','content-length':bytes.length,'accept-ranges':'bytes',etag:'"team-fixture"'});res.end(req.method==='HEAD'?undefined:bytes)})
await new Promise(resolve=>fixture.listen(0,'127.0.0.1',resolve))
const url=`http://127.0.0.1:${fixture.address().port}/file`
const cliEnv={...process.env,ZEUS_STATE_DIR:join(home,'zeus','state'),ZEUS_CONFIG_DIR:join(home,'zeus','config'),ZEUS_RUNTIME_DIR:join(home,'zeus','run'),ZEUS_PROFILE:`zeus-app-${createHash('sha256').update(home.toLowerCase()).digest('hex').slice(0,8)}`}
const cli=args=>new Promise((resolve,reject)=>{const child=spawn(join(BIN,process.platform==='win32'?'zeus.exe':'zeus'),['--json',...args],{env:cliEnv,windowsHide:true,stdio:['ignore','pipe','pipe']});let stdout='',stderr='';child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b);child.once('error',reject);child.once('exit',code=>resolve({code,stdout,stderr}));const timer=setTimeout(()=>{child.kill();reject(new Error('CLI fixture timed out'))},30000);timer.unref();child.once('exit',()=>clearTimeout(timer))})
let h,ui
try {
 h=await startHarness({home});await h.ready()
 const state=await h.post('/native/v1/team/status')
 check('native Team verifies a Node-service lease without signing in',state.configured&&state.trusted&&!state.connected&&state.policy.sequence===2&&!state.policy.features.downloads)
 check('team-held downloads leave read access available',Array.isArray((await h.get('/api/v1/jobs')).items))
 let denied=false;try{await h.post('/api/v1/links/preview',{items:[{url}]})}catch(e){denied=e.code==='team.policy_held'}
 check('desktop guard refuses new download effects before network access',denied&&requests===0)
 const blocked=await cli(['probe',url]);check('actual CLI request also reaches the daemon team gate',blocked.code!==0&&(blocked.stdout+blocked.stderr).includes('Team policy')&&requests===0)
 check('actual CLI list remains available under a restrictive policy',(await cli(['ls'])).code===0)
 ui=await openUi(h);const page=ui.page
 await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/team`)
 await page.getByRole('heading',{name:'Shared metadata',exact:true}).waitFor()
 check('unsigned-in metadata changes are disabled in the native page',await page.getByRole('button',{name:'Share selected metadata',exact:true}).isDisabled())
 for(const [width,height]of[[1440,900],[960,600],[390,740]]){await page.setViewportSize({width,height});await page.waitForTimeout(150);const d=await page.evaluate(()=>({w:document.documentElement.scrollWidth,h:document.documentElement.scrollHeight}));check(`Team panels remain contained at ${width}x${height}`,d.w<=width+1&&d.h<=height+1);await page.screenshot({path:join(output,`team-${width}.png`)})}
 check('Team page has no JavaScript failures',ui.errors.length===0)
 await ui.close();ui=null;await h.stop()
 writeFileSync(join(team,'policy.json'),JSON.stringify(publisher.policy(tenant,1,features(true),Date.now())))
 h=await startHarness({home});await h.ready()
 check('a signed older lease cannot undo restrictions after native restart',(await h.post('/native/v1/team/status')).policy_held)
 const rolled=await cli(['probe',url]);check('daemon replay floor also survives restart',rolled.code!==0&&(rolled.stdout+rolled.stderr).includes('Team policy')&&requests===0)
 writeFileSync(join(team,'policy.json'),JSON.stringify(publisher.policy(tenant,3,features(true),Date.now()-901000)))
 check('a newer but expired lease remains held',(await h.post('/native/v1/team/status')).policy_held)
 writeFileSync(join(team,'policy.json'),JSON.stringify(publisher.policy(tenant,3,features(true),Date.now())))
 check('a fresh newer signed lease can restore the existing local feature',(await h.post('/native/v1/team/status')).policy.sequence===3)
 const allowed=await cli(['probe',url]);check('download entitlement does not depend on cloud entitlement',allowed.code===0&&requests>0)
 writeFileSync(join(output,'result.json'),JSON.stringify({checks,approvals_made:0,live_identity_provider:false,fixture_requests:requests,platform:process.platform},null,2))
}finally{await ui?.close();await h?.stop();await new Promise(resolve=>fixture.close(resolve))}
