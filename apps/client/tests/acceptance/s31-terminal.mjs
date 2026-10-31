// Actual Windows terminal rendering and authenticated profile sharing, using a generated fixture only.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { startFixtures, sha256 } from '../fixture-server.mjs'
import { startHarness, download, until, BIN } from '../harness.mjs'
import { openUi } from '../ui.mjs'
if (process.platform !== 'win32') throw new Error('This console acceptance scenario requires Windows.')
if (!process.env.ZEUS_TERMINAL_EVIDENCE) throw new Error('Set ZEUS_TERMINAL_EVIDENCE to a fresh directory.')
const output=resolve(process.env.ZEUS_TERMINAL_EVIDENCE), home=join(output,'profile')
if (existsSync(output)) throw new Error('Existing evidence is preserved; choose a fresh directory.')
mkdirSync(output,{recursive:true})
const powershell=join(process.env.SystemRoot,'System32','WindowsPowerShell','v1.0','powershell.exe')
const helper=fileURLToPath(new URL('../terminal-probe.ps1',import.meta.url))
const fx=await startFixtures(0)
let h,ui,completed,opened=false
const probe=(close=false)=>{
 const report=join(output,close?'closed.json':'console.json')
 execFileSync(powershell,['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',helper,'-ExpectedTui',join(BIN,'zeus-tui.exe'),'-DesktopHome',h.home,'-ReportPath',report,...(close?['-Close']:[])],{windowsHide:true,stdio:'ignore',timeout:15000})
 return JSON.parse(readFileSync(report,'utf8').replace(/^\uFEFF/,''))
}
try {
 h=await startHarness({home});await h.ready()
 const size=65536,seed=79
 const created=await download(h,[{url:`${fx.url}/files/terminal-proof.pdf?size=${size}&seed=${seed}`}])
 const id=created.jobs[0].job_id
 const job=await until(async()=>{const j=(await h.get('/api/v1/jobs')).items.find(j=>j.id===id);return j?.state==='completed'&&j},{what:'fixture download'})
 assert.equal(sha256(seed,size), (await import('node:crypto')).createHash('sha256').update(readFileSync(job.path)).digest('hex'))
 ui=await openUi(h)
 await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/settings/daemon`)
 const launched=ui.page.waitForResponse(r=>r.url().endsWith('/native/v1/cloud/review-terminal'))
 await ui.page.getByRole('button',{name:'Open TUI',exact:true}).click()
 const response=await launched;const result=await response.json()
 assert.equal(response.ok(),true,JSON.stringify(result));assert.equal(result.opened,true);opened=true
 const screen=await until(()=>{const r=probe();return r.readable&&r.text.includes('LOCAL ENGINE CONNECTED')&&r.text.includes('terminal-proof.pdf')&&r},{timeoutMs:30000,everyMs:500,what:'rendered TUI connected to the desktop fixture'})
 assert.ok(screen.text.replace(/[\s│]/g,'').includes(sha256(seed,size)), 'terminal displays the actual verified digest')
 assert.equal(ui.errors.length,0)
 completed={passed:true,profile:h.home,job_id:id,console_owner:screen.owner,console_child:screen.child,checks:['desktop button opens a real terminal','TUI renders connected state','desktop download appears in the terminal','download bytes independently verified','terminal displays the recorded digest','no UI errors','owned fixture terminal closed']}

} catch(error) {
 writeFileSync(join(output,'failure.json'),JSON.stringify({error:String(error),stderr:h?.stderr(),text:await ui?.page.locator('body').innerText().catch(()=> '')},null,2))
 throw error
} finally {
 try {
  if(opened) { const result=probe(true); assert.ok(result.closed || result.childCount === 0,'close only this harness-owned terminal tree') }
 } catch(error) {
  writeFileSync(join(output,'failure.json'),JSON.stringify({phase:'cleanup',error:String(error)},null,2))
  throw error
 } finally { await ui?.close();await h?.stop();await fx.close() }
}

writeFileSync(join(output,'result.json'),JSON.stringify(completed,null,2))
console.log('SCENARIO 31: PASSED (real Windows console, shared profile, verified download)')
