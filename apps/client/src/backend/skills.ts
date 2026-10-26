// Agent skills: named instructions with a narrowed tool list and the access they need. A skill can only narrow what a
// thread may do: its tools are cut to the closed table (model/agentTools.ts) and the thread's access still applies.
// Skills belong to this client; zeusd has no skills, and MCP hosts keep their own.

import type { AgentAccess } from '../model/types.ts'

export interface Skill {
  id: string
  name: string
  enabled: boolean
  builtin: boolean
  summary: string
  /** When the agent uses it: phrases in a request. */
  triggers: string[]
  instructions: string
  tools: string[]
  access: AgentAccess
}

export function defaultSkills(): Skill[] {
  return [
    {
      id: 'batch-from-page',
      name: 'Batch from a page',
      enabled: true,
      builtin: true,
      summary: 'Find the files a page links to, check them, and propose one reviewed batch.',
      triggers: ['from this page', 'all the pdfs', 'everything linked', 'batch from'],
      instructions: [
        'Ask zeus_workflow to plan from the page address (request action plan, start_urls with that address, a goal that names the file types wanted); never go more than one page deep unless asked.',
        'If the daemon cannot draft a plan, say so plainly and suggest the Link grabber or the Site grabber; do not invent links.',
        'Probe at most three of the links with zeus_probe to confirm size and type.',
        'List what you found in your reply and ask which to start. Do not enqueue more than the person asked for.',
        'Say how many links you skipped and why.',
      ].join('\n'),
      tools: ['zeus_workflow', 'zeus_probe', 'zeus_status'],
      access: 'organize',
    },
    {
      id: 'organise-downloads',
      name: 'Organise downloads',
      enabled: true,
      builtin: true,
      summary: 'Preview moving files into folders by type, then ask for a review.',
      triggers: ['organise', 'organize', 'tidy', 'sort my downloads'],
      instructions: [
        'List files with zeus_files_list, then preview the rules with zeus_files (request action organize, dry_run true).',
        'Show every move as before and after. Never request the operation without the preview.',
        'Quarantined files stay where they are.',
      ].join('\n'),
      tools: ['zeus_files_list', 'zeus_files'],
      access: 'organize',
    },
    {
      id: 'verify-checksums',
      name: 'Verify checksums',
      enabled: true,
      builtin: true,
      summary: 'Re-hash finished files and compare them with what was recorded and published.',
      triggers: ['verify', 'checksum', 'sha256', 'hashes'],
      instructions: ['Use zeus_verify for each completed file asked about.', 'Report matches and mismatches in a table; a mismatch is never called verified.'].join('\n'),
      tools: ['zeus_files_list', 'zeus_verify', 'zeus_status'],
      access: 'read',
    },
    {
      id: 'mirror-release',
      name: 'Mirror a release',
      enabled: true,
      builtin: true,
      summary: 'Collect the assets of a release page with their published checksums.',
      triggers: ['mirror', 'latest release', 'release assets'],
      instructions: ['Sample the release page, pair each asset with its published SHA-256 when the page lists one, and propose the batch with expected hashes.', 'Skip source archives unless asked.'].join('\n'),
      tools: ['zeus_workflow', 'zeus_probe'],
      access: 'organize',
    },
    {
      id: 'watch-rss',
      name: 'Watch an RSS feed',
      enabled: false,
      builtin: true,
      summary: 'Check a feed on a schedule and queue new enclosures for review.',
      triggers: ['rss', 'feed', 'podcast'],
      instructions: ['Feeds are not built in this release: say so, and offer to batch the enclosures of the current feed page instead.'].join('\n'),
      tools: ['zeus_probe'],
      access: 'read',
    },
    {
      id: 'rescue-failed',
      name: 'Rescue a failed download',
      enabled: true,
      builtin: true,
      summary: 'Read why a download failed and propose the next step.',
      triggers: ['failed', 'why did', 'stuck', 'expired'],
      instructions: ['Read the job with zeus_status. Explain the error code in plain words and propose one next step as a card.', 'Never retry more than once without asking.'].join('\n'),
      tools: ['zeus_status', 'zeus_control', 'zeus_probe'],
      access: 'read',
    },
  ]
}
