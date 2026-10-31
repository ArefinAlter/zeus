import assert from 'node:assert/strict'
import { test } from 'node:test'
import { assertWindowsSystemImports, windowsImports } from '../../scripts/windows-imports.mjs'

// Minimal PE32+ image with a file-backed import section and a separate delay table.
function fixture(normal = 'KERNEL32.dll', delayed, virtualDelay = false) {
  const b = Buffer.alloc(0x600), pe = 0x80, optional = pe + 24, section = optional + 240
  b.writeUInt16LE(0x5a4d, 0); b.writeUInt32LE(pe, 0x3c)
  b.writeUInt32LE(0x4550, pe); b.writeUInt16LE(0x8664, pe + 4)
  b.writeUInt16LE(1, pe + 6); b.writeUInt16LE(240, pe + 20)
  b.writeUInt16LE(0x20b, optional); b.writeBigUInt64LE(0x100000n, optional + 24)
  b.writeUInt32LE(0x400, optional + 60); b.writeUInt32LE(16, optional + 108)
  b.writeUInt32LE(0x1000, optional + 120); b.writeUInt32LE(40, optional + 124)
  b.writeUInt32LE(0x200, section + 8); b.writeUInt32LE(0x1000, section + 12)
  b.writeUInt32LE(0x200, section + 16); b.writeUInt32LE(0x400, section + 20)
  b.writeUInt32LE(0x1100, 0x40c); b.write(normal, 0x500)
  if (delayed) {
    b.writeUInt32LE(0x1040, optional + 112 + 13 * 8); b.writeUInt32LE(64, optional + 116 + 13 * 8)
    b.writeUInt32LE(virtualDelay ? 0 : 1, 0x440)
    b.writeUInt32LE(0x1140 + (virtualDelay ? 0x100000 : 0), 0x444); b.write(delayed, 0x540)
  }
  return b
}

test('system DLLs and Windows API sets pass in both import directories', () => {
  assert.deepEqual(assertWindowsSystemImports(fixture('KERNEL32.dll', 'api-ms-win-crt-runtime-l1-1-0.dll'), 'app'), ['api-ms-win-crt-runtime-l1-1-0.dll', 'kernel32.dll'])
  assert.deepEqual(windowsImports(fixture('KERNEL32.dll', 'USER32.dll', true)), ['kernel32.dll', 'user32.dll'])
  assert.doesNotThrow(() => assertWindowsSystemImports(fixture('msvcrt.dll'), 'helper'))
})
test('VC runtime and other unshipped dependencies block release, including delay loads', () => {
  for (const dll of ['VCRUNTIME140.dll', 'VCRUNTIME140_1.dll', 'MSVCP140.dll', 'libgcc_s_seh-1.dll', 'unknown.dll']) {
    assert.throws(() => assertWindowsSystemImports(fixture(dll), 'sidecar'), /unbundled or unreviewed DLLs/)
    assert.throws(() => assertWindowsSystemImports(fixture('KERNEL32.dll', dll), 'sidecar'), /unbundled or unreviewed DLLs/)
  }
})
test('malformed/truncated/unmapped tables cannot be treated as dependency-free', () => {
  const base = fixture()
  for (const length of [0, 1, 64, 256, 1024, 1284]) assert.throws(() => windowsImports(base.subarray(0, length)))
  for (const offset of [0x40c, 0x110]) {
    const b = fixture(); b.writeUInt32LE(0xfffffff0, offset)
    assert.throws(() => windowsImports(b))
  }
  const empty = fixture(); empty.fill(0, 0x400, 0x414); assert.throws(() => windowsImports(empty))
  const unterminated = fixture(); unterminated.writeUInt32LE(20, 0x114); assert.throws(() => windowsImports(unterminated))
  const invalidAttributes = fixture('KERNEL32.dll', 'USER32.dll'); invalidAttributes.writeUInt32LE(2, 0x440)
  assert.throws(() => windowsImports(invalidAttributes))
  assert.throws(() => windowsImports(fixture('../runtime.dll')))
})
