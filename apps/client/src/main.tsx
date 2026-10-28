import { render } from 'preact'

import { App } from './app'
import { startClock } from './backend/store'
import { getPrefs } from './lib/prefs'
import { applyPrefs } from './lib/themes'
import './styles/tokens.css'
import './styles/app.css'
import './styles/settings.css'
import './styles/chrome.css'
import './styles/agent.css'
import './styles/zeus.css'
import './styles/layout.css'

const root = document.getElementById('app')
if (root) {
  applyPrefs(getPrefs())
  startClock()
  render(<App />, root)
}
