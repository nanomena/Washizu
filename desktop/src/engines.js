// @ts-check
// Decision engines are shared by every account. The chooser is the one owner
// of each engine host: it starts `akagi-core engine-host` when the first
// account that uses an engine opens, holds the host's stdin for as long as the
// application runs, and closes it when the application quits. See
// src/engine/PROTOCOL.md. No Electron here, so it is tested with plain Node.
import { EventEmitter } from 'node:events'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'

const CORE_NAME = process.platform === 'win32' ? 'akagi-core.exe' : 'akagi-core'
/** Wait before starting a host that stopped by itself again, doubled each time up to the longest. */
const RETRY_FIRST_MS = 2_000
const RETRY_LONGEST_MS = 60_000
/** How long a host has to stop its engine once its stdin closes. */
const STOP_WAIT_MS = 10_000

/** The core's executable, as game-main resolves it.
 * @param {{ value: string, packaged: boolean, resources: string, here: string }} where */
export function corePath({ value, packaged, resources, here }) {
  return path.resolve(value || (packaged ? path.join(resources, 'core', CORE_NAME) : path.join(here, '..', '..', 'target', 'release', CORE_NAME)))
}

/** @param {string} name */
function plainName(name) {
  return typeof name === 'string' && /^[^\\/:\x00-\x1f]+$/.test(name) && name !== '.' && name !== '..'
}

/** Whether `name` is an installed engine package under `engines`.
 * @param {string} engines @param {string} name */
export function isEngine(engines, name) {
  return plainName(name) && fs.existsSync(path.join(engines, name, 'engine.toml'))
}

/** One string key of a TOML section, as a core-written config holds it.
 * @param {string} text @param {string} section @param {string} key */
function tomlString(text, section, key) {
  const body = section
    ? text.match(new RegExp(`^\\s*\\[${section}\\][^\\S\\r\\n]*(?:#[^\\r\\n]*)?\\r?\\n([\\s\\S]*?)(?=^\\s*\\[|$(?![\\s\\S]))`, 'm'))?.[1]
    : text.split(/^\s*\[/m)[0]
  return body?.match(new RegExp(`^\\s*${key}\\s*=\\s*"([^"]*)"\\s*(?:#[^\\r\\n]*)?$`, 'm'))?.[1] ?? null
}

/** The engine an account's bot slots name, four-player first; null when they name none.
 * @param {string} home @param {string} engines */
export function accountEngine(home, engines) {
  const file = path.join(home, 'config.toml')
  const text = fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : ''
  for (const key of ['active_4p', 'active_3p']) {
    const name = tomlString(text, 'bot', key)
    if (name && isEngine(engines, name)) return name
  }
  return null
}

/** The engine new accounts use: `engines.toml`'s default, when it is installed.
 * @param {string} engines */
export function defaultEngine(engines) {
  const file = path.join(engines, 'engines.toml')
  if (!fs.existsSync(file)) return null
  const name = tomlString(fs.readFileSync(file, 'utf8'), '', 'default')
  return name && isEngine(engines, name) ? name : null
}

/** @typedef {{ child: import('node:child_process').ChildProcess | null, delay: number, timer: NodeJS.Timeout | null, started: number }} Host */

export class EngineHosts extends EventEmitter {
  /** @param {{ core: string, engines: string, spawn?: typeof spawn, retryMs?: number }} settings */
  constructor(settings) {
    super()
    this.settings = settings
    /** @type {Map<string, Host>} */
    this.hosts = new Map()
    this.stopping = false
  }

  /** Whether no host process is running. */
  get idle() { return [...this.hosts.values()].every(host => host.child === null) }

  /** Start the engine's host unless it already runs. Opening more accounts that use it
   * shares the same host.
   * @param {string} name */
  ensure(name) {
    if (this.stopping) throw new Error('应用正在退出。')
    if (!isEngine(this.settings.engines, name)) throw new Error(`没有安装引擎 ${name}。`)
    if (this.hosts.has(name)) return
    const host = { child: null, delay: this.settings.retryMs ?? RETRY_FIRST_MS, timer: null, started: 0 }
    this.hosts.set(name, host)
    this.#start(name, host)
  }

  /** @param {string} name @param {Host} host */
  #start(name, host) {
    host.timer = null
    if (this.stopping) return
    const dir = path.join(this.settings.engines, name)
    const logs = path.join(dir, 'logs')
    fs.mkdirSync(logs, { recursive: true })
    let child
    try {
      child = (this.settings.spawn ?? spawn)(this.settings.core, ['engine-host', '--engine', dir], { stdio: ['pipe', 'ignore', 'pipe'], windowsHide: true })
    } catch (cause) {
      this.emit('failure', name, cause instanceof Error ? cause.message : String(cause))
      this.#retry(name, host)
      return
    }
    host.child = child
    host.started = Date.now()
    let said = ''
    child.stdin?.on('error', () => {})
    child.stderr?.setEncoding('utf8').on('data', text => { said = (said + text).slice(-2_000) })
    child.on('error', error => { said ||= error.message })
    child.once('close', code => {
      if (host.child !== child) return
      host.child = null
      this.emit('change')
      if (this.stopping) return
      // It stopped by itself: an engine is shared, so it is started again.
      this.emit('failure', name, `引擎宿主已退出（${code}）${said ? `：${said}` : ''}`)
      if (Date.now() - host.started >= RETRY_LONGEST_MS) host.delay = this.settings.retryMs ?? RETRY_FIRST_MS
      this.#retry(name, host)
    })
    this.emit('change')
  }

  /** @param {string} name @param {Host} host */
  #retry(name, host) {
    const delay = host.delay
    host.delay = Math.min(host.delay * 2, RETRY_LONGEST_MS)
    host.timer = setTimeout(() => this.#start(name, host), delay)
  }

  /** Close every host's stdin and wait until each has stopped its engine and exited. */
  async stopAll() {
    this.stopping = true
    await Promise.all([...this.hosts.values()].map(host => {
      if (host.timer) { clearTimeout(host.timer); host.timer = null }
      const child = host.child
      if (!child) return Promise.resolve()
      return new Promise(done => {
        const timer = setTimeout(() => { child.kill(); done(undefined) }, STOP_WAIT_MS)
        child.once('close', () => { clearTimeout(timer); done(undefined) })
        child.stdin?.end()
      })
    }))
  }
}
