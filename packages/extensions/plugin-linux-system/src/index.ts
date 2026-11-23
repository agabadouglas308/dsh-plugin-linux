/**
 * Cordis plugin: linux-system
 *
 * Native Linux system capabilities for DeepSeek Harness.
 * Registers 7 tools: systemd (status, action), journalctl, clipboard (get, set),
 * containers, and desktop notifications.
 */
import type { Context } from '@deepseek-ai/cordis'
import { defineTool } from '@deepseek-ai/dsh-tools'
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import net from 'node:net'

const execFileP = promisify(execFile)

export const name = 'plugin-linux-system'
export const inject = ['tools']

type Scope = 'system' | 'user'
/** Lossless JSON value accepted by tool outputs. */
type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue }
/** Subset of the harness ToolRunContext this plugin consumes. */
type ToolExec = { signal?: AbortSignal }

interface RunOptions {
  /** Cooperative cancellation forwarded from the agent's tool call. */
  signal?: AbortSignal
}

async function runCmd(
  cmd: string,
  args: string[],
  timeoutMs = 15_000,
  options: RunOptions = {},
): Promise<{ stdout: string; stderr: string }> {
  if (options.signal?.aborted) throw new Error(`${cmd} aborted before start`)
  const { stdout, stderr } = await execFileP(cmd, args, {
    timeout: timeoutMs,
    maxBuffer: 8 * 1024 * 1024,
    signal: options.signal,
  })
  return { stdout: String(stdout), stderr: String(stderr) }
}

function spawnWithStdin(
  cmd: string,
  args: string[],
  input: string,
  { timeoutMs = 5_000, signal }: { timeoutMs?: number; signal?: AbortSignal } = {},
): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new Error(`${cmd} aborted before start`))
      return
    }
    const child = spawn(cmd, args, { stdio: ['pipe', 'ignore', 'pipe'] })
    const errChunks: Buffer[] = []
    let stdinError: Error | null = null
    let settled = false
    const finish = (error?: Error): void => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      signal?.removeEventListener('abort', onAbort)
      if (error) reject(error)
      else resolve()
    }
    const onAbort = (): void => {
      child.kill('SIGKILL')
      finish(new Error(`${cmd} aborted`))
    }
    const timer = setTimeout(() => {
      child.kill('SIGKILL')
      finish(new Error(`${cmd} timed out after ${timeoutMs}ms`))
    }, timeoutMs)
    signal?.addEventListener('abort', onAbort, { once: true })
    child.stderr?.on('data', (d: Buffer) => errChunks.push(d))
    // Clipboard helpers routinely exit before consuming every byte. An EPIPE
    // on stdin is expected then; the close code decides success. Any other
    // stream error must still fail the tool call instead of crashing dsh.
    child.stdin?.on('error', (e: NodeJS.ErrnoException) => {
      if (e.code !== 'EPIPE') stdinError = e
    })
    child.on('error', (e) => finish(e))
    child.on('close', (code) => {
      if (code === 0) { finish(stdinError ?? undefined); return }
      finish(new Error(`${cmd} exited ${code}: ${Buffer.concat(errChunks).toString('utf8').trim()}`))
    })
    child.stdin?.end(input)
  })
}

function scopeArgs(scope: Scope): string[] {
  return scope === 'user' ? ['--user'] : []
}

async function readClipboard(options: RunOptions = {}): Promise<string> {
  const errors: string[] = []
  if (process.env.WAYLAND_DISPLAY) {
    try { return (await runCmd('wl-paste', ['--no-newline', '--type', 'text/plain'], 3000, options)).stdout } catch (e) { errors.push(String(e)) }
  }
  try { return (await runCmd('xclip', ['-selection', 'clipboard', '-o'], 3000, options)).stdout } catch (e) { errors.push(String(e)) }
  try { return (await runCmd('xsel', ['--clipboard', '--output'], 3000, options)).stdout } catch (e) { errors.push(String(e)) }
  throw new Error('No clipboard reader available (wl-paste / xclip / xsel). Errors:\n' + errors.join('\n'))
}

async function writeClipboard(text: string, options: RunOptions = {}): Promise<void> {
  const errors: string[] = []
  if (process.env.WAYLAND_DISPLAY) {
    try { await spawnWithStdin('wl-copy', ['--type', 'text/plain'], text, options); return } catch (e) { errors.push(String(e)) }
  }
  try { await spawnWithStdin('xclip', ['-selection', 'clipboard'], text, options); return } catch (e) { errors.push(String(e)) }
  try { await spawnWithStdin('xsel', ['--clipboard', '--input'], text, options); return } catch (e) { errors.push(String(e)) }
  throw new Error('No clipboard writer available (wl-copy / xclip / xsel). Errors:\n' + errors.join('\n'))
}

async function sendNotification(args: {
  summary: string
  body?: string
  urgency?: 'low' | 'normal' | 'critical'
}, options: RunOptions = {}): Promise<{ ok: true; via: 'socket' | 'notify-send' }> {
  const payload = JSON.stringify({
    summary: args.summary,
    body: args.body ?? '',
    urgency: args.urgency ?? 'normal',
  })
  const sockPath = process.env.DSH_NOTIFY_SOCK
  if (sockPath) {
    try {
      await new Promise<void>((resolve, reject) => {
        const c = net.createConnection(sockPath)
        let settled = false
        const finish = (error?: Error): void => {
          if (settled) return
          settled = true
          clearTimeout(timer)
          options.signal?.removeEventListener('abort', onAbort)
          if (error) { c.destroy(); reject(error) } else resolve()
        }
        const onAbort = (): void => {
          c.destroy()
          finish(new Error('notification aborted'))
        }
        const timer = setTimeout(() => finish(new Error('notification socket timed out')), 3000)
        options.signal?.addEventListener('abort', onAbort, { once: true })
        c.on('connect', () => { c.write(payload + '\n'); c.end() })
        c.on('end', () => finish())
        c.on('close', () => finish())
        c.on('error', (e) => finish(e))
      })
      return { ok: true, via: 'socket' }
    } catch {
      // fall through to notify-send
    }
  }
  const urgency = args.urgency === 'critical' ? 'critical' : args.urgency === 'low' ? 'low' : 'normal'
  await execFileP('notify-send', ['-a', 'DeepSeek Harness', '-u', urgency, args.summary, args.body ?? ''], {
    timeout: 5000,
    signal: options.signal,
  })
  return { ok: true, via: 'notify-send' }
}

export function apply(ctx: Context): void {
  // ── systemd: status ─────────────────────────────────────────────────────
  ctx.tools.register(defineTool({
    name: 'linux_systemd_status',
    description:
      'Return structured status of a systemd unit: LoadState, ActiveState, SubState, UnitFileState, MainPID, Description, FragmentPath, ExecMainStartTimestamp. Default scope is system; pass scope="user" for --user units.',
    parameters: {
      unit: { type: 'string', required: true, description: 'Unit name, e.g. "nginx.service" or "ssh.socket".' },
      scope: { type: 'string', enum: ['system', 'user'], description: '"system" (default) or "user".' },
    },
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    isConcurrencySafe: () => true,
    async execute(args: { unit: string; scope?: Scope }, exec: ToolExec = {}) {
      const scope: Scope = args.scope ?? 'system'
      const props = 'LoadState,ActiveState,SubState,UnitFileState,MainPID,Description,FragmentPath,ExecMainStartTimestamp'
      const { stdout } = await runCmd('systemctl', [...scopeArgs(scope), 'show', '--property=' + props, '--no-pager', '--', args.unit], 15_000, exec)
      const info: Record<string, string> = {}
      for (const line of stdout.trim().split('\n')) {
        const eq = line.indexOf('=')
        if (eq > 0) info[line.slice(0, eq)] = line.slice(eq + 1)
      }
      return { unit: args.unit, scope, properties: info }
    },
  }))

  // ── systemd: action ─────────────────────────────────────────────────────
  ctx.tools.register(defineTool({
    name: 'linux_systemd_action',
    description: 'Start, stop, restart, enable, or disable a systemd unit. Mutating — runs serially.',
    parameters: {
      unit: { type: 'string', required: true, description: 'Unit name.' },
      action: { type: 'string', required: true, enum: ['start', 'stop', 'restart', 'enable', 'disable'], description: 'Action to perform.' },
      scope: { type: 'string', enum: ['system', 'user'], description: '"system" (default) or "user".' },
    },
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    isConcurrencySafe: () => false,
    async execute(args: { unit: string; action: 'start' | 'stop' | 'restart' | 'enable' | 'disable'; scope?: Scope }, exec: ToolExec = {}) {
      const scope: Scope = args.scope ?? 'system'
      const { stdout, stderr } = await runCmd('systemctl', [...scopeArgs(scope), args.action, '--', args.unit], 15_000, exec)
      return { unit: args.unit, action: args.action, scope, stdout: stdout.trim(), stderr: stderr.trim() }
    },
  }))

  // ── journalctl ──────────────────────────────────────────────────────────
  ctx.tools.register(defineTool({
    name: 'linux_journal',
    description: 'Read structured journalctl entries. Optionally filter by unit and time window. Returns one JSON object per journal entry.',
    parameters: {
      unit: { type: 'string', description: 'Filter by systemd unit (optional).' },
      lines: { type: 'integer', description: 'Max entries to return (1-10000). Default 100.' },
      since: { type: 'string', description: 'Time window, e.g. "10 min ago" or "2024-01-01 12:00".' },
    },
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    isConcurrencySafe: () => true,
    async execute(args: { unit?: string; lines?: number; since?: string }, exec: ToolExec = {}) {
      // Keep journalctl bounded even if the model asks for an extreme count.
      const lines = Math.min(Math.max(Math.trunc(args.lines ?? 100), 1), 10_000)
      const argv = ['--no-pager', '--output=json', '-n', String(lines)]
      if (args.unit) argv.push('-u', args.unit)
      if (args.since) argv.push('--since', args.since)
      const { stdout } = await runCmd('journalctl', argv, 20_000, exec)
      const entries: JsonValue[] = []
      for (const line of stdout.trim().split('\n')) {
        if (!line) continue
        try { entries.push(JSON.parse(line) as JsonValue) } catch { entries.push({ MESSAGE: line }) }
      }
      return { unit: args.unit ?? null, count: entries.length, entries }
    },
  }))

  // ── clipboard: get ──────────────────────────────────────────────────────
  ctx.tools.register(defineTool({
    name: 'linux_clipboard_get',
    description: 'Read plain text from the system clipboard. Uses Wayland (wl-paste) if WAYLAND_DISPLAY is set, otherwise X11 (xclip / xsel).',
    parameters: {},
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    isConcurrencySafe: () => true,
    async execute(_args: unknown, exec: ToolExec = {}) {
      const text = await readClipboard(exec)
      return { text, length: text.length }
    },
  }))

  // ── clipboard: set ──────────────────────────────────────────────────────
  ctx.tools.register(defineTool({
    name: 'linux_clipboard_set',
    description: 'Write plain text to the system clipboard. Same backend selection as linux_clipboard_get.',
    parameters: {
      text: { type: 'string', required: true, description: 'Text to place on the clipboard.' },
    },
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    isConcurrencySafe: () => false,
    async execute(args: { text: string }, exec: ToolExec = {}) {
      await writeClipboard(args.text, exec)
      return { ok: true, length: args.text.length }
    },
  }))

  // ── containers ──────────────────────────────────────────────────────────
  ctx.tools.register(defineTool({
    name: 'linux_containers',
    description: 'List running containers via podman (preferred) or docker. Returns the runtime used and the parsed container list.',
    parameters: {},
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    isConcurrencySafe: () => true,
    async execute(_args: unknown, exec: ToolExec = {}) {
      const errors: string[] = []
      for (const runtime of ['podman', 'docker'] as const) {
        try {
          const { stdout } = await runCmd(runtime, ['ps', '--format', 'json'], 10_000, exec)
          let containers: JsonValue
          try { containers = JSON.parse(stdout) as JsonValue } catch { containers = stdout.trim() }
          return { runtime, containers }
        } catch (e) { errors.push(`${runtime}: ${String(e)}`) }
      }
      throw new Error('neither podman nor docker is available:\n' + errors.join('\n'))
    },
  }))

  // ── notifications ───────────────────────────────────────────────────────
  ctx.tools.register(defineTool({
    name: 'linux_notify',
    description:
      'Send a native desktop notification (D-Bus). Uses the wrapper socket when DSH_NOTIFY_SOCK is set, else falls back to notify-send. Urgency: low / normal / critical.',
    parameters: {
      summary: { type: 'string', required: true, description: 'Notification title.' },
      body: { type: 'string', description: 'Notification body text.' },
      urgency: { type: 'string', enum: ['low', 'normal', 'critical'], description: 'Notification urgency (default: normal).' },
    },
    output: {
      schema: { type: 'json' },
      render: (_args, value) => [{ type: 'text', text: JSON.stringify(value, null, 2) }],
    },
    isConcurrencySafe: () => true,
    async execute(args: { summary: string; body?: string; urgency?: 'low' | 'normal' | 'critical' }, exec: ToolExec = {}) {
      return await sendNotification(args, exec)
    },
  }))
}