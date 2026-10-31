// The composer: where a person gives the agent a task. Enter sends, Shift+Enter breaks the line, `/` lists ready-made
// requests. The access picker sets what the agent may do in this thread; it cannot add a tool that does not exist.

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import { NATIVE } from '../../backend/live'
import { ACCESS_INFO, ACCESS_ORDER } from '../../model/agentTools.ts'
import type { AgentAccess } from '../../model/types.ts'
import { Icon } from '../../ui/icons'
import type { IconName } from '../../ui/icons'
import { Popover } from '../../ui/Popover'
import { ModelPicker } from './ModelPicker'

const drafts = new Map<string, string>()

const COMMANDS: { name: string; hint: string; text: string }[] = [
  { name: 'status', hint: 'What is downloading and what waits for you', text: 'What is downloading, and what is waiting for me?' },
  { name: 'rescue', hint: 'Why a download failed', text: 'Why did the map tiles download fail?' },
  { name: 'batch', hint: 'Files linked from a page', text: 'Get all the PDFs from https://papers.example.org/2026/' },
  { name: 'login', hint: 'A file behind a site login', text: 'Download the conference proceedings from the portal (it needs my login)' },
  { name: 'verify', hint: 'Re-hash finished files', text: 'Verify the finished files' },
  { name: 'organise', hint: 'Preview moving files by type', text: 'Organise my downloads by type' },
  { name: 'mirror', hint: 'Assets of a release', text: 'Mirror the latest release from https://releases.example.com/toolkit/' },
  { name: 'deliver', hint: 'Plan a cloud delivery', text: 'Send the annual report to my Google Drive' },
  { name: 'approve', hint: 'Ask the agent to approve (it cannot)', text: 'Approve the portal request for me' },
]

export function Composer({ draftKey, busy, access, onAccess, onSend, onStop, placeholder, providerId, onProvider }: { draftKey: string; busy: boolean; access: AgentAccess; onAccess: (a: AgentAccess) => void; onSend: (text: string) => void; onStop: () => void; placeholder?: string; providerId?: string | undefined; onProvider?: (providerId: string) => void }) {
  const [text, setText] = useState(drafts.get(draftKey) ?? '')
  const [menu, setMenu] = useState<string | null>(null)
  const [sel, setSel] = useState(0)
  const area = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    setText(drafts.get(draftKey) ?? '')
    setMenu(null)
    area.current?.focus()
  }, [draftKey])
  useEffect(() => {
    drafts.set(draftKey, text)
  }, [draftKey, text])
  useEffect(() => {
    const el = area.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, 220)}px`
  }, [text])

  const items = useMemo(() => (menu === null ? [] : COMMANDS.filter((c) => c.name.startsWith(menu))), [menu])
  useEffect(() => setSel(0), [menu])

  const send = () => {
    const t = text.trim()
    if (!t || busy) return
    onSend(t)
    setText('')
    setMenu(null)
    drafts.delete(draftKey)
  }
  const choose = (i: number) => {
    const c = items[i]
    if (!c) return
    setText(c.text)
    setMenu(null)
    requestAnimationFrame(() => area.current?.focus())
  }
  const onKeyDown = (e: KeyboardEvent) => {
    if (menu !== null && items.length) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault()
        setSel((s) => Math.max(0, Math.min(items.length - 1, s + (e.key === 'ArrowDown' ? 1 : -1))))
        return
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault()
        choose(sel)
        return
      }
      if (e.key === 'Escape') {
        e.preventDefault()
        setMenu(null)
        return
      }
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault()
      send()
    }
  }
  const accessIcon: IconName = access === 'read' ? 'eye' : access === 'download' ? 'download' : 'layers'
  return (
    <div class="composer-wrap">
      <div class={`composer ${busy ? 'busy' : ''}`}>
        <textarea
          ref={area}
          rows={2}
          value={text}
          placeholder={placeholder ?? 'Ask for downloads, files or checks, paste a link, or / for ready-made requests'}
          aria-label="Message to the agent"
          onInput={(e) => {
            const v = (e.target as HTMLTextAreaElement).value
            setText(v)
            const m = /^\/([a-z]*)$/.exec(v)
            setMenu(m ? (m[1] ?? '') : null)
          }}
          onKeyDown={onKeyDown}
        />
        {menu !== null && items.length ? (
          <div class="mentionmenu" role="listbox" aria-label="Ready-made requests">
            {items.map((c, i) => (
              <button type="button" role="option" aria-selected={i === sel} class={i === sel ? 'on' : ''} onMouseEnter={() => setSel(i)} onMouseDown={(e) => (e.preventDefault(), choose(i))}>
                <Icon name="sparkle" />
                <span class="name">/{c.name}</span>
                <span class="meta">{c.hint}</span>
              </button>
            ))}
          </div>
        ) : null}
        <div class="cmp-bar">
          {NATIVE ? (
            <ModelPicker providerId={providerId} onPick={(id) => onProvider?.(id)} />
          ) : (
          <Popover label="Model: Zeus agent, prepared answers" title="This release answers from a script; no model is called" trigger={<><Icon name="sparkle" /> <span class="cbl">Zeus agent · prepared</span> <Icon name="chevron" /></>}>
            {(close) => (
              <>
                <div class="pophead">Model</div>
                <button type="button" role="menuitemradio" aria-checked class="on" onClick={close}>
                  <span class="name">Zeus agent, prepared answers</span>
                  <span class="desc">Answers come from a script that calls the same tools; no model is called and nothing leaves this computer.</span>
                </button>
                <button type="button" role="menuitemradio" aria-checked={false} disabled>
                  <span class="name">Your provider ladder</span>
                  <span class="desc">Needs a real zeusd with a provider configured. Settings, Providers.</span>
                </button>
              </>
            )}
          </Popover>
          )}
          <span class="vsep" />
          <Popover label={`Access: ${ACCESS_INFO[access].label}`} trigger={<><Icon name={accessIcon} /> <span class="cbl">{ACCESS_INFO[access].label}</span> <Icon name="chevron" /></>}>
            {(close) => (
              <>
                <div class="pophead">What the agent may do in this thread</div>
                {ACCESS_ORDER.map((a) => (
                  <button type="button" role="menuitemradio" aria-checked={a === access} class={a === access ? 'on' : ''} onClick={() => (onAccess(a), close())}>
                    <span class="name">{ACCESS_INFO[a].label}</span>
                    <span class="desc">{ACCESS_INFO[a].blurb}</span>
                  </button>
                ))}
                <div class="popfoot">At any access it cannot approve a request, see a cookie or a password, or open a downloaded file.</div>
              </>
            )}
          </Popover>
          <span class="spacer" />
          {busy ? (
            <button type="button" class="sendbtn stop" aria-label="Stop the agent" title="Stop" onClick={onStop}>
              <Icon name="stop" />
            </button>
          ) : (
            <button type="button" class="sendbtn" aria-label="Send" title="Send (Enter)" disabled={!text.trim()} onClick={send}>
              <Icon name="send" />
            </button>
          )}
        </div>
      </div>
      <div class="cmp-note">{NATIVE ? "Your selected model answers using its installed-agent sign-in or API connection. Check its advice; proposed changes need your approval." : "The agent's words are not checked by Zeus. It cannot approve a request."}</div>
    </div>
  )
}
