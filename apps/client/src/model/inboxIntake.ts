/** Builds an atomic, private intake request. It contains no dispatch or approval decision. */
export type IntakeLink = {url:string;name:string;dir:string;pkg:string;check:string;expectedSha256?:string;dupeChoice?:string}
export function packageForLinks(links: readonly IntakeLink[]) {
  const retained = links.filter(link => link.dupeChoice !== 'skip')
  if (!retained.length || retained.length > 200) throw new Error('Select 1 to 200 requests that are not marked Skip.')
  const initial_entries = retained.map(link => {
    let parsed:URL
    try { parsed = new URL(link.url) } catch { throw new Error('A selected request has an invalid address.') }
    if (!['https:', 'http:'].includes(parsed.protocol)) throw new Error('Only HTTP and HTTPS requests can be saved here.')
    return {job: {
      url:link.url,
      ...(link.name ? {filename:link.name} : {}),
      ...(link.dir ? {dest_dir:link.dir} : {}),
      ...(link.expectedSha256 ? {expected_sha256:link.expectedSha256} : {}),
      ...(link.check==='needs_login' ? {session_origin:parsed.origin} : {}),
      overwrite:'fail' as const,
    }, untrusted_label:link.pkg || null}
  })
  return {untrusted_name:'Link grabber selection',initial_entries}
}
/** Same typed selection recovers its existing owner-scoped package after a lost response or reload. */
export async function intakeKey(request: ReturnType<typeof packageForLinks>) {
  const bytes=new TextEncoder().encode(JSON.stringify(request))
  if(bytes.byteLength>512*1024)throw new Error('The selected requests exceed the package intake limit.')
  const digest=await crypto.subtle.digest('SHA-256',bytes)
  return 'grabber-intake-'+Array.from(new Uint8Array(digest),v=>v.toString(16).padStart(2,'0')).join('')
}
