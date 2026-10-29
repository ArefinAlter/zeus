import {test} from 'node:test'
import assert from 'node:assert/strict'
import {packageForLinks,intakeKey} from './inboxIntake.ts'
const link={url:'https://example.org/report.pdf?token=ZEUS-CANARY-intake',name:'report.pdf',dir:'D:/Downloads',pkg:'Papers',check:'needs_login',expectedSha256:'a'.repeat(64)}
test('intake preserves exact sources, explicit names, destinations and hashes without dispatch',()=>{
 const request=packageForLinks([link,{...link,url:'https://example.org/ignored',dupeChoice:'skip'}]);assert.equal(request.initial_entries.length,1)
 const item=request.initial_entries[0];assert.equal(item.job.url,link.url);assert.equal(item.job.filename,link.name);assert.equal(item.job.dest_dir,link.dir);assert.equal(item.job.expected_sha256,link.expectedSha256);assert.equal(item.job.session_origin,'https://example.org');assert.equal(item.job.overwrite,'fail');assert.equal(item.untrusted_label,'Papers');assert.equal('submit' in request,false)
 assert.equal('session_origin' in packageForLinks([{...link,check:'online'}]).initial_entries[0].job,false)
})
test('lost-response intake keys survive reload but distinguish changed private requests',async()=>{
 const request=packageForLinks([link]);const first=await intakeKey(request);assert.equal(first,await intakeKey(JSON.parse(JSON.stringify(request))));assert.match(first,/^grabber-intake-[a-f0-9]{64}$/);assert(!first.includes('CANARY'))
 for(const changed of [{url:link.url+'2'},{dir:'D:/Other'},{name:'other.pdf'},{expectedSha256:'b'.repeat(64)}])assert.notEqual(first,await intakeKey(packageForLinks([{...link,...changed}])))
})
test('invalid schemes, empty selections and oversized batches fail before retaining metadata',()=>{
 assert.throws(()=>packageForLinks([]));assert.throws(()=>packageForLinks([{...link,dupeChoice:'skip'}]));assert.throws(()=>packageForLinks(Array(201).fill(link)));assert.throws(()=>packageForLinks([{...link,url:'file:///secret'}]));assert.throws(()=>packageForLinks([{...link,url:'not an address'}]))
})
