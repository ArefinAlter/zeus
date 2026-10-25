// Settings: one page per concern. The order follows T3 Code's settings where they overlap (General, Appearance,
// Keybindings, Providers), then the options of a download manager and of a file search tool, then agent, storage,
// security and the daemon link. The shell shows this list in place of the sidebar.

import { href } from '../../lib/router'
import { Empty } from '../../ui/components'
import type { IconName } from '../../ui/icons'
import { About } from '../About'
import { AppearanceSettings } from './Appearance'
import { KeybindingSettings } from './Keybindings'
import { ProvidersSettings } from './Providers'
import {
  AgentSettings,
  BrowserSettings,
  ConnectionSettings,
  DaemonSettings,
  DownloadsSettings,
  FileTypesSettings,
  GeneralSettings,
  IndexSettings,
  LimiterSettings,
  NotificationSettings,
  SaveToSettings,
  SchedulerSettings,
  SecuritySettings,
  SiteLoginsSettings,
  StorageSettings,
} from './Sections'

export interface SettingsSection {
  id: string
  label: string
  icon: IconName
  hint: string
}

export const SETTINGS_SECTIONS: SettingsSection[] = [
  { id: 'general', label: 'General', icon: 'sliders', hint: 'Start page, tray, confirmations' },
  { id: 'appearance', label: 'Appearance', icon: 'palette', hint: 'Scheme, themes, accent, density' },
  { id: 'keybindings', label: 'Keybindings', icon: 'keyboard', hint: 'Shortcuts and when they fire' },
  { id: 'filetypes', label: 'File types', icon: 'layers', hint: 'Extensions and categories' },
  { id: 'saveto', label: 'Save to', icon: 'folder', hint: 'Folders and write roots' },
  { id: 'downloads', label: 'Downloads', icon: 'download', hint: 'Connections, retries, resume, duplicates' },
  { id: 'connection', label: 'Connection and proxy', icon: 'globe', hint: 'Network, proxy, SOCKS' },
  { id: 'sitelogins', label: 'Site logins', icon: 'key', hint: 'Sites that may use your session' },
  { id: 'browser', label: 'Browser integration', icon: 'browser', hint: 'The extension and handoffs' },
  { id: 'scheduler', label: 'Scheduler', icon: 'clock', hint: 'Queue defaults' },
  { id: 'limiter', label: 'Speed limiter', icon: 'gauge', hint: 'Global and per-queue limits' },
  { id: 'notifications', label: 'Notifications', icon: 'bell', hint: 'What to show, quiet hours, sounds' },
  { id: 'index', label: 'Search and index', icon: 'search', hint: 'Indexed folders, exclusions, rescans' },
  { id: 'providers', label: 'Providers', icon: 'sparkle', hint: 'Model ladder, your own keys' },
  { id: 'agent', label: 'Agent', icon: 'chat', hint: 'Access, tools, instructions' },
  { id: 'storage', label: 'Storage and cleanup', icon: 'archive', hint: 'Retention and cleanup preview' },
  { id: 'security', label: 'Security', icon: 'shield', hint: 'Global stop and fixed rules' },
  { id: 'daemon', label: 'Daemon', icon: 'server', hint: 'Which zeusd this app uses' },
  { id: 'about', label: 'About', icon: 'info', hint: 'Pre-alpha release, what is real' },
]

export function Settings({ section }: { section: string }) {
  switch (section) {
    case 'general':
      return <GeneralSettings />
    case 'appearance':
      return <AppearanceSettings />
    case 'keybindings':
      return <KeybindingSettings />
    case 'filetypes':
      return <FileTypesSettings />
    case 'saveto':
      return <SaveToSettings />
    case 'downloads':
      return <DownloadsSettings />
    case 'connection':
      return <ConnectionSettings />
    case 'sitelogins':
      return <SiteLoginsSettings />
    case 'browser':
      return <BrowserSettings />
    case 'scheduler':
      return <SchedulerSettings />
    case 'limiter':
      return <LimiterSettings />
    case 'notifications':
      return <NotificationSettings />
    case 'index':
      return <IndexSettings />
    case 'providers':
      return <ProvidersSettings />
    case 'agent':
      return <AgentSettings />
    case 'storage':
      return <StorageSettings />
    case 'security':
      return <SecuritySettings />
    case 'daemon':
      return <DaemonSettings />
    case 'about':
      return <About />
    default:
      return (
        <Empty>
          No such settings page. <a href={href('settings', 'general')}>General</a>
        </Empty>
      )
  }
}
