// Approvals are decided by a person at a trust surface of Zeus: the interactive command line (a real terminal) or the
// browser extension's popup. This shows the exact command for this app's engine, so a person can paste it.

import { useEffect, useState } from 'preact/hooks'

import { approvalsCommand } from '../../model/agentEvents.ts'
import { NATIVE } from '../../backend/live'
import { native, session } from '../../backend/remote'
import { CodeCopy } from '../../ui/components'

let cached: { env: Record<string, string>; bin: string; platform: string } | null = null

export function ApprovalCommand() {
  const [info, setInfo] = useState(cached)
  useEffect(() => {
    if (!NATIVE || cached) return
    void Promise.all([native.engine(), session()]).then(([e, s]) => {
      cached = { env: e.env, bin: e.bin, platform: s.platform ?? 'linux' }
      setInfo(cached)
    })
  }, [])
  if (!NATIVE || !info) return null
  return (
    <div class="approval-command">
      <span class="muted small">To decide it in a terminal (a person at a real terminal, never this chat):</span>
      <CodeCopy text={approvalsCommand(info.env, info.bin, info.platform)} />
    </div>
  )
}
