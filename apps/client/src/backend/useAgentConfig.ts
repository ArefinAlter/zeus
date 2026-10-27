// A hook for the agent configuration that the engine holds (provider ladder, consent, keystore, spend).

import { useEffect, useState } from 'preact/hooks'

import type { AgentConfigView } from './agentNative.ts'
import { currentConfig, refreshConfig, subscribeConfig } from './agentNative.ts'

/** The configuration, loaded once and refreshed whenever any page saves it. `null` until the engine answers. */
export function useAgentConfig(): AgentConfigView | null {
  const [c, setC] = useState(currentConfig())
  useEffect(() => {
    const off = subscribeConfig(() => setC(currentConfig()))
    if (!currentConfig()) void refreshConfig().catch(() => undefined)
    return off
  }, [])
  return c
}
