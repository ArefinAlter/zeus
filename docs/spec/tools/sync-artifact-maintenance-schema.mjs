// Regenerate only the typed artifact maintenance profile after UPDATE_SCHEMAS=1 cargo test -p zeus-api schema.
import {readFileSync,writeFileSync} from 'node:fs'
import {fileURLToPath} from 'node:url'
import {dirname,resolve} from 'node:path'
const root=resolve(dirname(fileURLToPath(import.meta.url)),'../../..')
const read=path=>JSON.parse(readFileSync(resolve(root,path),'utf8'))
const write=(path,value)=>writeFileSync(resolve(root,path),JSON.stringify(value,null,2)+'\n')
const draft=read('crates/zeus-api/schemas/daemon-api.v1.draft.schema.json')
const names=['artifacts.maintenance.preview','artifacts.maintenance.apply','artifacts.events','artifacts.maintenance.status']
const methods=names.map(name=>[name,draft.methods[name]])
if(methods.some(([,shape])=>!shape))throw new Error('regenerate the typed API schema first')
const selected=new Set()
function collect(node){if(!node||typeof node!=='object')return;if(typeof node.$ref==='string'&&node.$ref.startsWith('#/$defs/')){const name=node.$ref.slice(8);if(!selected.has(name)){if(!draft.$defs[name])throw new Error(`missing ${name}`);selected.add(name);collect(draft.$defs[name])}}Object.values(node).forEach(collect)}
methods.forEach(([,shape])=>{collect(shape.params);collect(shape.result)})
const defs=Object.fromEntries([...selected].sort().map(name=>[name,structuredClone(draft.$defs[name])]))
function normalize(node){if(!node||typeof node!=='object')return;if(/^uint(8|16|32|64)$/.test(node.format??'')){const bits=Number(node.format.slice(4));node.minimum=Math.max(node.minimum??0,0);node.maximum=Math.min(node.maximum??Number.MAX_SAFE_INTEGER,bits===64?Number.MAX_SAFE_INTEGER:2**bits-1);delete node.format}Object.values(node).forEach(normalize)}
normalize(defs)
const id='https://zeus.invalid/schema/artifact-maintenance.v1'
write('docs/spec/schema/artifact-maintenance.v1.schema.json',{$schema:'https://json-schema.org/draft/2020-12/schema',$id:id,title:'Zeus artifact maintenance v1',description:'Exact reviewed bounded bundles, collection, integrity/repair and scoped audit replay. Never grants approval authority.', $defs:defs})
const api=read('docs/spec/schema/daemon-api.v1.schema.json')
const external=node=>JSON.parse(JSON.stringify(node).replaceAll('"#/$defs/',`"${id}#/$defs/`))
for(const[method,shape]of methods){const name=method.split('.').map(word=>word[0].toUpperCase()+word.slice(1)).join('');api.$defs[name+'Params']=external(shape.params);api.$defs[name+'Result']=external(shape.result);api.$defs[name+'Request']={allOf:[{$ref:'#/$defs/RequestBase'},{type:'object',properties:{method:{const:method},params:{$ref:`#/$defs/${name}Params`}},required:['method','params']}]};api.$defs[name+'Response']={allOf:[{$ref:'#/$defs/SuccessBase'},{type:'object',properties:{result:{$ref:`#/$defs/${name}Result`}}}]}}
write('docs/spec/schema/daemon-api.v1.schema.json',api)
// Terminal approvals embed the typed plan/dependencies so they do not require a network schema resolver.
const approvalPath='docs/spec/schema/approvals.v1.schema.json'
let approvalText=readFileSync(resolve(root,approvalPath),'utf8').replace(/,\n    "MaintenanceApi_[\s\S]*\n  }\n}\s*$/,'\n  }\n}\n')
selected.clear();collect({$ref:'#/$defs/ArtifactMaintenancePlan'})
const extra={}
for(const name of selected)extra['MaintenanceApi_'+name]=JSON.parse(JSON.stringify(defs[name]).replaceAll('"#/$defs/','"#/$defs/MaintenanceApi_'))
if(!approvalText.includes('"maintenance_plan":'))approvalText=approvalText.replace('"artifact_plan": { "$ref": "#/$defs/ArtifactLifecyclePlan" },','"artifact_plan": { "$ref": "#/$defs/ArtifactLifecyclePlan" },\n            "maintenance_plan": { "$ref": "#/$defs/MaintenanceApi_ArtifactMaintenancePlan" },')
const ending=approvalText.lastIndexOf('\n  }\n}')
if(ending<0)throw new Error('unexpected approvals schema structure')
const extraText=JSON.stringify(extra,null,2).slice(2,-2).split('\n').map(line=>'  '+line).join('\n')
writeFileSync(resolve(root,approvalPath),approvalText.slice(0,ending)+',\n'+extraText+approvalText.slice(ending))
