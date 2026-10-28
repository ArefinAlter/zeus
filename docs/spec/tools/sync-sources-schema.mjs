// Refresh only immutable-source types in the normative API after UPDATE_SCHEMAS=1 cargo test -p zeus-api schema.
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'../../..');
const read=p=>JSON.parse(readFileSync(resolve(root,p),'utf8'));
const write=(p,v)=>{const s=JSON.stringify(v,null,2)+'\n';if(process.argv.includes('--check')){if(readFileSync(resolve(root,p),'utf8').replaceAll('\r\n','\n')!==s)throw Error(`stale source schema: ${p}`);}else writeFileSync(resolve(root,p),s);};
const draft=read('crates/zeus-api/schemas/daemon-api.v1.draft.schema.json');
const methods=Object.entries(draft.methods).filter(([n])=>n.startsWith('sources.'));
if(methods.length!==8)throw Error('Regenerate the typed API schema first; expected eight source methods');
const names=new Set();
function collect(v){if(!v||typeof v!=='object')return;if(typeof v.$ref==='string'&&v.$ref.startsWith('#/$defs/')){const n=v.$ref.slice(8);if(!names.has(n)){names.add(n);collect(draft.$defs[n]);}}Object.values(v).forEach(collect);}
methods.forEach(([,v])=>{collect(v.params);collect(v.result);});
const defs=Object.fromEntries([...names].sort().map(n=>[n,structuredClone(draft.$defs[n])]));
function normalize(v){if(!v||typeof v!=='object')return;if(/^uint(8|16|32|64)$/.test(v.format??'')){const bits=Number(v.format.slice(4));v.minimum=Math.max(v.minimum??0,0);v.maximum=Math.min(v.maximum??Number.MAX_SAFE_INTEGER,bits===64?Number.MAX_SAFE_INTEGER:2**bits-1);delete v.format;}Object.values(v).forEach(normalize);}
normalize(defs);
for(const v of Object.values(defs))for(const [name,p]of Object.entries(v.properties??{})){
 if(['credential','metainfo_base64','idempotency_key'].includes(name))Object.assign(p,{writeOnly:true,'x-zeus-secret':true});
 if(name==='idempotency_key')Object.assign(p,{minLength:1,maxLength:128});
 if(name==='metainfo_base64')Object.assign(p,{maxLength:1048576});
 if(name==='account_id')Object.assign(p,{minLength:1,maxLength:64});
}
const id='https://zeus.invalid/schema/sources.v1';
write('docs/spec/schema/sources.v1.schema.json',{$schema:'https://json-schema.org/draft/2020-12/schema',$id:id,title:'Zeus immutable source acquisition v1',description:'Human-configured account scopes; owner-bound immutable reviews, held HTTP admission and bounded torrent acquisition. Credentials and metainfo are write-only; no scope is granted by client labels.',$defs:defs});
const external=v=>JSON.parse(JSON.stringify(v).replaceAll('"#/$defs/',`"${id}#/$defs/`));
const api=read('docs/spec/schema/daemon-api.v1.schema.json');
for(const [method,v]of methods){const name=method.split('.').map(s=>s[0].toUpperCase()+s.slice(1)).join('');api.$defs[name+'Params']=external(v.params);api.$defs[name+'Result']=external(v.result);api.$defs[name+'Request']={allOf:[{$ref:'#/$defs/RequestBase'},{type:'object',properties:{method:{const:method},params:{$ref:`#/$defs/${name}Params`}},required:['method','params']}]};api.$defs[name+'Response']={allOf:[{$ref:'#/$defs/SuccessBase'},{type:'object',properties:{result:{$ref:`#/$defs/${name}Result`}}}]};}
write('docs/spec/schema/daemon-api.v1.schema.json',api);
