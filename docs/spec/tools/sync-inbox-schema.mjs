// Add only the typed intake profile to the normative API, preserving other independently owned profiles.
// Run after UPDATE_SCHEMAS=1 cargo test -p zeus-api schema.
import { readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
const root=resolve(dirname(fileURLToPath(import.meta.url)),'../../..')
const read=path=>JSON.parse(readFileSync(resolve(root,path),'utf8'))
const check=process.argv.includes('--check')
const write=(path,value)=>{const text=JSON.stringify(value,null,2)+'\n';if(check){if(readFileSync(resolve(root,path),'utf8').replaceAll('\r\n','\n')!==text)throw new Error(`stale inbox schema: ${path}`)}else writeFileSync(resolve(root,path),text)}
const draft=read('crates/zeus-api/schemas/daemon-api.v1.draft.schema.json')
const methods=Object.entries(draft.methods).filter(([name])=>name.startsWith('inbox.'))
const expected=['inbox.packages.put','inbox.packages.list','inbox.packages.get','inbox.packages.archive','inbox.entries.add','inbox.entries.remove','inbox.preview','inbox.submit','inbox.submissions.get','inbox.submissions.retry'].sort()
if(JSON.stringify(methods.map(([name])=>name).sort())!==JSON.stringify(expected))throw new Error('regenerate the typed API schema first; expected ten intake methods')
const selected=new Set()
function collect(node){if(!node||typeof node!=='object')return;if(typeof node.$ref==='string'&&node.$ref.startsWith('#/$defs/')){const name=node.$ref.slice(8);if(!selected.has(name)){if(!draft.$defs[name])throw new Error(`missing ${name}`);selected.add(name);collect(draft.$defs[name])}}Object.values(node).forEach(collect)}
methods.forEach(([,shape])=>{collect(shape.params);collect(shape.result)})
const defs=Object.fromEntries([...selected].sort().map(name=>[name,structuredClone(draft.$defs[name])]))
function normalize(node){if(!node||typeof node!=='object')return;if(/^uint(8|16|32|64)$/.test(node.format??'')){const bits=Number(node.format.slice(4));node.minimum=Math.max(node.minimum??0,0);node.maximum=Math.min(node.maximum??Number.MAX_SAFE_INTEGER,bits===64?Number.MAX_SAFE_INTEGER:2**bits-1);delete node.format}Object.values(node).forEach(normalize)}
normalize(defs)
for(const definition of Object.values(defs))for(const [name,property]of Object.entries(definition.properties??{})){
 if(name==='owner_credential_id')Object.assign(property,{'x-zeus-human-only':true,pattern:'^cli_[0-7][0-9A-HJKMNP-TV-Z]{25}$'})
 if(name==='idempotency_key')Object.assign(property,{minLength:1,maxLength:128,'x-zeus-secret':true,writeOnly:true})
 if(name==='untrusted_name')Object.assign(property,{minLength:1,maxLength:160})
 if(name==='untrusted_label')Object.assign(property,{maxLength:500})
 if(name==='limit')Object.assign(property,{minimum:1,maximum:Number.MAX_SAFE_INTEGER})
}
for(const name of ['url','hint_url','referrer','extra_headers'])if(defs.NewJob?.properties?.[name])Object.assign(defs.NewJob.properties[name],{'x-zeus-secret':true,writeOnly:true})
if(defs.InboxIntakeItem?.properties?.job)Object.assign(defs.InboxIntakeItem.properties.job,{writeOnly:true,'x-zeus-secret':true})
Object.assign(defs.InboxEntriesAddParams.properties.entries,{minItems:1,maxItems:200})
Object.assign(defs.InboxEntriesRemoveParams.properties.entry_ids,{minItems:1,maxItems:500,uniqueItems:true})
Object.assign(defs.InboxPreviewParams.properties.entry_ids,{maxItems:50,uniqueItems:true})
const id='https://zeus.invalid/schema/inbox.v1'
write('docs/spec/schema/inbox.v1.schema.json',{$schema:'https://json-schema.org/draft/2020-12/schema',$id:id,title:'Zeus durable intake v1',description:'Owner-scoped durable packages, exact intake deduplication, immutable50-job review and recoverable enqueue receipts. Original requests remain private; intake confers no approval or root grant. Byte limits are enforced by the daemon.', $defs:defs})
const api=read('docs/spec/schema/daemon-api.v1.schema.json')
const external=node=>JSON.parse(JSON.stringify(node).replaceAll('"#/$defs/',`"${id}#/$defs/`))
for(const [method,shape]of methods){
 const name=method.split('.').map(word=>word[0].toUpperCase()+word.slice(1)).join('')
 api.$defs[name+'Params']=external(shape.params);api.$defs[name+'Result']=external(shape.result)
 api.$defs[name+'Request']={allOf:[{$ref:'#/$defs/RequestBase'},{type:'object',properties:{method:{const:method},params:{$ref:`#/$defs/${name}Params`}},required:['method','params']}]}
 api.$defs[name+'Response']={allOf:[{$ref:'#/$defs/SuccessBase'},{type:'object',properties:{result:{$ref:`#/$defs/${name}Result`}}}]}
}
write('docs/spec/schema/daemon-api.v1.schema.json',api)
