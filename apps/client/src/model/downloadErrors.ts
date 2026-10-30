import type { Job } from './types.ts'

const ERRORS: Record<string, [string, string]> = {
  'http.forbidden': ['The server refused the file (403). A signed link that expired looks like this.', 'Get a fresh link from the site and add it again.'],
  'url.expired': ['The download link expired.', 'Get a fresh link from the site and add it again.'],
  'http.blocked_challenge': ['The site requires a browser check before downloading.', 'Open the site in your browser, then use the Zeus extension to hand off the download.'],
  'net.connect_timeout': ['The server did not answer in time.', 'Check the address and connection; Zeus will retry according to its retry policy.'],
  'net.read_timeout': ['The server stopped sending data.', 'Zeus will retry; use a fresh link if the server keeps refusing the transfer.'],
  'http.auth_required': ['The site requires a login.', 'Sign in through your browser and use the Zeus extension.'],
  'http.status_429': ['The site has temporarily limited downloads.', 'Wait for the retry time or reduce concurrent downloads from this site.'],
  'http.url_expired': ['The link expired.', 'Get a fresh link from the site and add it again.'],
  'http.not_found': ['The server has no such file (404).', 'Check the address.'],
  'http.gone': ['The file was removed from the server (410).', 'Nothing to retry; find another source.'],
  'http.server_error': ['The server failed (5xx).', 'Retry later.'],
  'http.unavailable': ['The server is busy (503) and asked Zeus to wait.', 'zeusd retries on its own.'],
}

const STATUS: Record<string, string> = { '403': 'http.forbidden', '404': 'http.not_found', '410': 'http.gone', '500': 'http.server_error', '502': 'http.server_error', '503': 'http.unavailable' }

export function errorOf(code: string | undefined): Job['error'] | undefined {
  if (!code) return undefined
  const status = /^http\.status_(\d{3})$/.exec(code)?.[1]
  const known = ERRORS[code] ?? ERRORS[status ? (STATUS[status] ?? '') : code] ?? (code.startsWith('http.') ? ['The server answered with an error.', 'Retry, or check the address.'] : ['zeusd reported a problem with this download.', 'Retry, or remove it from the list.'])
  return { code, message: known[0], next: known[1] }
}
