// A minimal MCP client over stdio (JSON-RPC 2.0, newline-delimited), written for the acceptance runs. It is not an SDK:
// it starts a server process, performs the handshake and makes requests, exactly as an assistant would.
//
//   const c = await McpClient.start({ command, args: ['mcp'], env })
//   const init = await c.initialize()
//   const tools = await c.listTools()
//   const result = await c.callTool('zeus_probe', { url })
//   await c.close()

import { spawn } from 'node:child_process'

export class McpClient {
  constructor(child) {
    this.child = child
    this.nextId = 1
    this.pending = new Map()
    this.notifications = []
    this.stderr = ''
    this.buffer = ''
    child.stdout.on('data', (d) => {
      this.buffer += d
      let i
      while ((i = this.buffer.indexOf('\n')) >= 0) {
        const line = this.buffer.slice(0, i).trim()
        this.buffer = this.buffer.slice(i + 1)
        if (!line) continue
        let msg
        try {
          msg = JSON.parse(line)
        } catch {
          continue
        }
        if (msg.id !== undefined && this.pending.has(msg.id)) {
          const { resolve, reject } = this.pending.get(msg.id)
          this.pending.delete(msg.id)
          if (msg.error) reject(Object.assign(new Error(msg.error.message), { code: msg.error.code, data: msg.error.data }))
          else resolve(msg.result)
        } else this.notifications.push(msg)
      }
    })
    child.stderr.on('data', (d) => (this.stderr += d))
    child.on('exit', (code) => {
      for (const { reject } of this.pending.values()) reject(new Error(`the server exited (${code}): ${this.stderr.slice(-300)}`))
      this.pending.clear()
    })
  }

  static start({ command, args = [], env = {} }) {
    const child = spawn(command, args, { env: { ...process.env, ...env }, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
    return new McpClient(child)
  }

  request(method, params = {}, timeoutMs = 30000) {
    const id = this.nextId++
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`${method} timed out`))
      }, timeoutMs)
      this.pending.set(id, {
        resolve: (v) => (clearTimeout(t), resolve(v)),
        reject: (e) => (clearTimeout(t), reject(e)),
      })
      this.child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n')
    })
  }

  notify(method, params) {
    this.child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method, ...(params ? { params } : {}) }) + '\n')
  }

  async initialize() {
    const result = await this.request('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'zeus-acceptance-client', version: '0.0.1' } })
    this.notify('notifications/initialized')
    return result
  }

  async listTools() {
    return (await this.request('tools/list')).tools
  }

  /** The tool result's structured content when present, else its text parsed as JSON when possible. */
  async callTool(name, args = {}, timeoutMs = 60000) {
    const result = await this.request('tools/call', { name, arguments: args }, timeoutMs)
    const text = result.content?.map((c) => c.text ?? '').join('\n') ?? ''
    return { isError: !!result.isError, structured: result.structuredContent ?? null, text, raw: result }
  }

  async close() {
    this.child.stdin.end()
    await new Promise((r) => {
      const t = setTimeout(() => (this.child.kill(), r()), 3000)
      this.child.once('exit', () => (clearTimeout(t), r()))
    })
  }
}
