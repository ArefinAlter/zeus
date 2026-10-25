// Skills: named instructions for the agent with a narrowed tool list. A skill can only narrow what a thread may do:
// its tools come from the closed table of Zeus's MCP tools and the thread's access still applies.

import { useState } from 'preact/hooks'

import type { Skill } from '../backend/skills'
import { addSkill, deleteSkill, updateSkill, useStore } from '../backend/store'
import { ACCESS_INFO, ACCESS_ORDER, NEVER, TOOLS, accessRank, clampSkillTools } from '../model/agentTools.ts'
import type { AgentAccess } from '../model/types.ts'
import { href, navigate } from '../lib/router'
import { Chip, PageHead, Provenance, Switch } from '../ui/components'
import { Icon } from '../ui/icons'

function needed(tools: string[]): AgentAccess {
  let r = 0
  for (const t of tools) {
    const info = TOOLS.find((x) => x.name === t)
    if (info) r = Math.max(r, accessRank(info.access))
  }
  return ACCESS_ORDER[r] ?? 'read'
}

export function Skills({ id }: { id?: string }) {
  const skills = useStore((s) => s.skills)
  const cur = skills.find((s) => s.id === id) ?? skills[0]
  const create = () => {
    const sid = `skill-${Date.now().toString(36)}`
    addSkill({ id: sid, name: 'New skill', enabled: false, builtin: false, summary: 'What this skill is for.', triggers: [], instructions: '', tools: ['zeus_probe', 'zeus_status'], access: 'read' })
    navigate(href('skills', sid))
  }
  return (
    <div class="page">
      <PageHead title="Skills" sub="Named instructions the agent follows when a request matches, each limited to the tools it lists.">
        <button type="button" class="btn" onClick={create}>
          <Icon name="plus" /> New skill
        </button>
      </PageHead>
      <div class="split-pane">
        <div class="pane-list" role="listbox" aria-label="Skills">
          {skills.map((s) => (
            <div class={`pane-item ${s.id === cur?.id ? 'on' : ''}`} role="option" aria-selected={s.id === cur?.id} tabIndex={0} onClick={() => navigate(href('skills', s.id))} onKeyDown={(e) => e.key === 'Enter' && navigate(href('skills', s.id))}>
              <Icon name="skill" />
              <span class="pi-text">
                <span class="pi-name">{s.name}</span>
                <span class="pi-meta">
                  {s.enabled ? 'on' : 'off'} · {clampSkillTools(s.tools).length} tools · needs {ACCESS_INFO[needed(s.tools)].label}
                </span>
              </span>
              <Switch label={`${s.name} enabled`} checked={s.enabled} onChange={(v) => updateSkill(s.id, { enabled: v })} />
            </div>
          ))}
        </div>
        <div class="pane-detail">{cur ? <SkillEditor skill={cur} key={cur.id} /> : null}</div>
      </div>
      <Provenance>Skills belong to this client. zeusd has no skills; an MCP host keeps its own. Whatever a skill says, the tool table and the fixed rules on this page stay the same.</Provenance>
    </div>
  )
}

function SkillEditor({ skill }: { skill: Skill }) {
  const [triggers, setTriggers] = useState(skill.triggers.join(', '))
  const tools = clampSkillTools(skill.tools)
  const need = needed(tools)
  return (
    <>
      <div class="pane-head">
        <div>
          <div class="title">
            {skill.name} {skill.builtin ? <Chip>built in</Chip> : <Chip tone="accent">yours</Chip>}
          </div>
          <div class="desc">{skill.summary}</div>
        </div>
        <div class="pane-tools">
          <Switch label="Enabled" checked={skill.enabled} onChange={(v) => updateSkill(skill.id, { enabled: v })} />
          {!skill.builtin ? (
            <button type="button" class="icon-btn" aria-label="Delete skill" onClick={() => (deleteSkill(skill.id), navigate(href('skills')))}>
              <Icon name="trash" />
            </button>
          ) : null}
        </div>
      </div>
      <div class="fields">
        <label class="field">
          Name
          <input value={skill.name} onChange={(e) => updateSkill(skill.id, { name: (e.target as HTMLInputElement).value.trim() || skill.name })} />
        </label>
        <label class="field">
          Used when a request mentions <span class="muted">comma separated</span>
          <input value={triggers} onInput={(e) => setTriggers((e.target as HTMLInputElement).value)} onBlur={() => updateSkill(skill.id, { triggers: triggers.split(',').map((t) => t.trim().toLowerCase()).filter(Boolean) })} />
        </label>
        <label class="field wide">
          Summary
          <input value={skill.summary} onChange={(e) => updateSkill(skill.id, { summary: (e.target as HTMLInputElement).value })} />
        </label>
        <label class="field wide">
          Instructions
          <textarea class="mono" rows={7} value={skill.instructions} onChange={(e) => updateSkill(skill.id, { instructions: (e.target as HTMLTextAreaElement).value })} />
        </label>
      </div>
      <section class="sgroup">
        <h2 class="sgroup-title">Tools it may call</h2>
        <div class="rows">
          {TOOLS.map((t) => (
            <label class="row toolrow">
              <input type="checkbox" checked={tools.includes(t.name)} onChange={(e) => updateSkill(skill.id, { tools: (e.target as HTMLInputElement).checked ? [...tools, t.name] : tools.filter((x) => x !== t.name) })} />
              <span class="text">
                <span class="title mono">{t.name}</span>
                <span class="desc">{t.summary}</span>
              </span>
              <span class="control">{t.access === 'read' ? <Chip>read</Chip> : <Chip tone="warn">{ACCESS_INFO[t.access].label}</Chip>}</span>
            </label>
          ))}
        </div>
        <p class="sgroup-note">
          With these tools the skill needs <strong>{ACCESS_INFO[need].label}</strong> access. In a thread with less, the calls that need more are refused and shown as refused. A skill cannot add a tool that is not in this table.
        </p>
      </section>
      <section class="sgroup">
        <h2 class="sgroup-title">Never, whatever the skill says</h2>
        <ul class="never">
          {NEVER.map((n) => (
            <li>
              <Icon name="shield" />
              {n}
            </li>
          ))}
        </ul>
      </section>
    </>
  )
}
