// Direct and delay-load PE imports only; dynamic LoadLibrary calls are outside this gate.
// The release target is Windows 10+ x64. Every permitted DLL is an OS component.
const systemDlls = new Set(`advapi32 bcrypt bcryptprimitives cabinet cfgmgr32 combase comctl32
comdlg32 crypt32 cryptbase cryptsp dbghelp dnsapi dwmapi gdi32 hid imm32 iphlpapi kernel32
mpr msimg32 msvcrt ncrypt netapi32 normaliz ntdll ole32 oleaut32 powrprof propsys psapi
rpcrt4 secur32 setupapi shell32 shlwapi ucrtbase user32 userenv uxtheme version winhttp
wininet winmm winspool wintrust ws2_32 wtsapi32`.split(/\s+/).map(name => `${name}.dll`))

export function windowsImports(bytes) {
  const fail = () => { throw new Error('Invalid or unsupported Windows x64 PE import table.') }
  const bounds = (offset, size) => {
    if (!Number.isSafeInteger(offset) || offset < 0 || size < 0 || offset + size > bytes.length) fail()
    return offset
  }
  const u16 = offset => bytes.readUInt16LE(bounds(offset, 2))
  const u32 = offset => bytes.readUInt32LE(bounds(offset, 4))
  if (u16(0) !== 0x5a4d) fail()
  const pe = u32(0x3c)
  if (u32(pe) !== 0x4550 || u16(pe + 4) !== 0x8664) fail()
  const count = u16(pe + 6), optionalSize = u16(pe + 20), optional = pe + 24
  if (!count || count > 96 || optionalSize < 112 || u16(optional) !== 0x20b) fail()
  bounds(optional, optionalSize)
  const imageBase = bytes.readBigUInt64LE(bounds(optional + 24, 8))
  const headerSize = u32(optional + 60), directoryCount = u32(optional + 108)
  if (112 + directoryCount * 8 > optionalSize || headerSize > bytes.length) fail()
  const sections = Array.from({ length: count }, (_, i) => {
    const at = bounds(optional + optionalSize + i * 40, 40)
    return { virtualSize: u32(at + 8), rva: u32(at + 12), size: u32(at + 16), offset: u32(at + 20) }
  })
  const map = (rva, size) => {
    if (!rva || !Number.isSafeInteger(rva) || rva < 0 || rva > 0xffffffff) fail()
    if (rva < headerSize && rva + size <= headerSize) return bounds(rva, size)
    const matches = sections.filter(s => rva >= s.rva && rva + size <= s.rva + (s.virtualSize || s.size))
    if (matches.length !== 1) fail()
    const section = matches[0], delta = rva - section.rva
    if (delta + size > section.size) fail()
    return bounds(section.offset + delta, size)
  }
  const nameAt = rva => {
    let name = ''
    for (let i = 0; i < 128; i++) {
      const byte = bytes[map(rva + i, 1)]
      if (!byte) {
        if (!/^[a-z0-9_-]+\.dll$/i.test(name)) fail()
        return name.toLowerCase()
      }
      if (byte < 0x21 || byte > 0x7e) fail()
      name += String.fromCharCode(byte)
    }
    fail()
  }
  const imports = new Set()
  for (const [index, stride, nameOffset] of [[1, 20, 12], [13, 32, 4]]) {
    if (directoryCount <= index) continue
    const entry = optional + 112 + index * 8, rva = u32(entry), size = u32(entry + 4)
    if (!rva && !size) continue
    if (!rva || size < stride || size > 1024 * 1024) fail()
    let terminated = false
    for (let offset = 0; offset + stride <= size; offset += stride) {
      const at = map(rva + offset, stride)
      if (bytes.subarray(at, at + stride).every(value => value === 0)) { terminated = true; break }
      let name = u32(at + nameOffset)
      if (index === 13) {
        const attributes = u32(at)
        if (attributes > 1) fail()
        if (!attributes) name = Number(BigInt(name) - imageBase)
      }
      imports.add(nameAt(name))
    }
    if (!terminated) fail()
  }
  // These shipped applications all import Windows functions; an empty result is suspicious.
  if (!imports.size) fail()
  return [...imports].sort()
}

export function assertWindowsSystemImports(bytes, label) {
  const imports = windowsImports(bytes)
  const unexpected = imports.filter(name => !systemDlls.has(name) && !/^(?:api|ext)-ms-win-[a-z0-9-]+\.dll$/.test(name))
  if (unexpected.length) throw new Error(`${label} requires unbundled or unreviewed DLLs: ${unexpected.join(', ')}`)
  return imports
}
