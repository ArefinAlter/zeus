// The sections of the sidebar, in groups. Categories, queues and agent threads are listed under them by the shell.

import type { IconName } from './ui/icons'

export interface NavItem {
  id: string
  label: string
  icon: IconName
  group: string
}

export const NAV: NavItem[] = [
  { id: 'downloads', label: 'Downloads', icon: 'download', group: 'Transfers' },
  { id: 'grabber', label: 'Link grabber', icon: 'grab', group: 'Transfers' },
  { id: 'queues', label: 'Queues and scheduler', icon: 'clock', group: 'Transfers' },
  { id: 'files', label: 'Files', icon: 'search', group: 'Transfers' },
  { id: 'agent', label: 'Agent', icon: 'chat', group: 'Agents' },
  { id: 'skills', label: 'Skills', icon: 'skill', group: 'Agents' },
  { id: 'approvals', label: 'Approvals', icon: 'seal', group: 'Trust' },
  { id: 'connectors', label: 'Connectors', icon: 'plug', group: 'Trust' },
]
