// Settings, Browser integration, for the desktop app's engine: real facts about the extension and the host, in place of
// the sample switches the other builds show. The extension keeps its own options (which downloads to offer, the
// minimum size, never-lists) in the browser; this page says so instead of showing switches that would change nothing.

import { href } from '../../lib/router'
import { Chip, Notice, PageHead, SettingRow, SettingsGroup } from '../../ui/components'
import { BrowserIntegration } from './BrowserIntegration'

export function BrowserIntegrationSettings() {
  return (
    <>
      <PageHead title="Browser integration" sub="The Zeus extension, and the host program that connects it to this app's engine." />
      <SettingsGroup title="The Zeus extension" note="The extension hands over downloads the browser gave up on, and supplies a site's session after you approve that site. It is the only path between your browser and Zeus.">
        <div class="row stack">
          <BrowserIntegration />
        </div>
      </SettingsGroup>
      <SettingsGroup title="Taking over downloads" note="These choices live in the extension, in the browser, because the extension is what sees the downloads.">
        <SettingRow title="Which failed downloads are offered" desc="Open the extension's options page in your browser (right-click its toolbar icon, then Options). It offers an interrupted download to Zeus after you approve; a download blocked by the browser's own safety checks is never offered." fixed>
          <Chip>set in the extension</Chip>
        </SettingRow>
        <SettingRow title="Approvals" desc="A handoff and any use of your login wait in Approvals. You decide them in the extension's popup or at a terminal; this app only lists them." fixed>
          <a class="btn small" href={href('approvals')}>
            Open approvals
          </a>
        </SettingRow>
      </SettingsGroup>
      <Notice>Zeus never reads your browser's cookies or profile folders. Session material moves only through the extension, in memory, for at most fifteen minutes, for a site you approved.</Notice>
    </>
  )
}
