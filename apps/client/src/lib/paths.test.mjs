import assert from 'node:assert/strict'
import { test } from 'node:test'
import { parentOf, joinPath, isInside, setSep } from './paths.ts'

test('drive roots stay absolute when browsing upwards or opening a file parent', () => {
  setSep('\\')
  assert.equal(parentOf('D:\\folder'), 'D:\\')
  assert.equal(parentOf('D:\\'), 'D:\\')
  assert.equal(joinPath(parentOf('D:\\file.txt'), 'file.txt'), 'D:\\file.txt')
  assert.ok(isInside('D:\\', parentOf('D:\\folder')))
  assert.ok(!isInside('D:\\allowed', 'D:\\allowed-other'))
})
test('network shares and Unix roots retain their root separators', () => {
  setSep('\\')
  assert.equal(parentOf('\\\\server\\share\\file'), '\\\\server\\share')
  setSep('/')
  assert.equal(parentOf('/file'), '/')
  assert.equal(joinPath('/', 'file'), '/file')
  assert.ok(isInside('/', '/folder'))
  setSep('\\')
})
