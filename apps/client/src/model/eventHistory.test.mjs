import assert from 'node:assert/strict'
import { test } from 'node:test'
import { historyLabel } from './eventHistory.ts'

test('only known event names and states become labels', () => {
  assert.equal(historyLabel('job.completed', null), 'Download completed')
  assert.equal(historyLabel('job.state_changed', 'awaiting_session'), 'zeusd: awaiting session')
  for (const kind of ['job.progress', 'future.change', '__proto__', 'constructor', 'ZEUS-CANARY-TEXT']) assert.equal(historyLabel(kind, null), null)
  assert.equal(historyLabel('job.state_changed', 'ZEUS-CANARY-STATE'), null)
})
