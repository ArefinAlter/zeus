// What will be sent, and the person's permission. Shown before a thread's first request to a provider: the system
// prompt, the thread's messages, the tool definitions and a token estimate, and what is never sent. Nothing leaves this
// computer until the person allows the provider here or in Settings, Providers.

import { useEffect, useState } from 'preact/hooks'

import { currentConsent, subscribeConsent } from '../../backend/agentNative'
import type { ConsentRequest } from '../../backend/agentNative'
import { href } from '../../lib/router'
import { Chip, Dialog, Notice } from '../../ui/components'
import '../../styles/agent-native.css'

export function useConsent(): ConsentRequest | null {
  const [c, setC] = useState(currentConsent())
  useEffect(() => subscribeConsent(() => setC(currentConsent())), [])
  return c
}

export function ConsentDialog() {
  const c = useConsent()
  if (!c) return null
  const p = c.preview
  const e = p.estimate
  return (
    <Dialog title={`Send this thread to ${p.provider.name}?`} onClose={() => c.resolve(false)} wide>
      <p>
        Zeus has not sent anything to <strong>{p.provider.name}</strong> yet. If you allow it, Zeus makes the request itself, from this computer, to{' '}
        <span class="mono">{p.provider.host}</span> with the key you stored{p.provider.local ? '. That address is on this computer or your own network' : ''}. The model <span class="mono">{p.provider.model}</span> answers with the tools below.
      </p>
      <div class="consent-grid">
        <div>
          <h4>Sent</h4>
          <ul class="plain">
            {p.sends.map((s) => (
              <li>{s}</li>
            ))}
          </ul>
        </div>
        <div>
          <h4>Never sent</h4>
          <ul class="plain">
            {p.never_sends.map((s) => (
              <li>{s}</li>
            ))}
          </ul>
        </div>
      </div>
      <p class="muted small">
        About {e.total_input_tokens.toLocaleString()} tokens in ({e.system_tokens.toLocaleString()} system prompt, {e.tool_tokens.toLocaleString()} tool definitions, {e.message_tokens.toLocaleString()} messages), up to {e.max_output_tokens.toLocaleString()} out. Access in this thread: {p.tool_access}.
      </p>
      <details>
        <summary>System prompt</summary>
        <pre class="consent-pre">{p.system}</pre>
      </details>
      <details>
        <summary>
          Messages <Chip>{p.messages.length}</Chip>
        </summary>
        <ul class="consent-msgs">
          {p.messages.map((m) => (
            <li>
              <span class="role">{m.role}</span> <span>{m.text.length > 600 ? `${m.text.slice(0, 600)}…` : m.text}</span>
            </li>
          ))}
        </ul>
      </details>
      <details>
        <summary>
          Tool definitions <Chip>{p.tools.length}</Chip>
        </summary>
        <ul class="consent-msgs">
          {p.tools.map((t) => (
            <li>
              <span class="role mono">{t.name}</span> <span>{t.description}</span>
            </li>
          ))}
        </ul>
      </details>
      <Notice>
        The model's answers are not checked by Zeus. It can call only these tools; none of them approves a request or reads a password, and anything that needs your approval waits for you. You can withdraw permission any time in <a href={href('settings', 'providers')}>Settings, Providers</a>.
      </Notice>
      <div class="decide-row">
        <button type="button" class="btn" onClick={() => c.resolve(false)}>
          Do not send
        </button>
        <button type="button" class="btn primary" onClick={() => c.resolve(true)}>
          Allow {p.provider.name} and send
        </button>
      </div>
    </Dialog>
  )
}
