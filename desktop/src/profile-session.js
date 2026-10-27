// @ts-check
// The chooser's sole client-lifecycle owner. Slots are keyed by profile id;
// release requires both the child's cleanup acknowledgement and process close.
import { EventEmitter } from 'node:events'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { chromiumSwitches, runName } from './journal.js'

/** @typedef {'starting' | 'active' | 'closing' | 'failed'} ClientPhase */
/** @typedef {'unconfirmed' | 'acknowledged' | 'not-started'} Cleanup */
/** @typedef {{id: string, state: ClientPhase, cleanup: Cleanup, error: string}} ClientState */
/** @typedef {ClientState & {child: import('node:child_process').ChildProcess | null}} Slot */

export class ProfileSessions extends EventEmitter {
  /** @param {{ executable: string, entry: string[], root: string, options?: string[], spawn?: typeof spawn }} settings */
  constructor(settings) {
    super()
    this.settings = settings
    /** @type {Map<string, Slot>} */
    this.clients = new Map()
  }

  /** Runtime protocol: absent ids are closed; failed/unconfirmed ids stay locked. */
  list() { return [...this.clients.values()].map(({ id, state, cleanup, error }) => ({ id, state, cleanup, error })) }
  get liveCount() { return [...this.clients.values()].filter(slot => slot.child !== null).length }

  /** The lifecycle owner alone decides whether a profile is safe to remove.
   * @param {string} id @param {() => void} remove */
  remove(id, remove) {
    const slot = this.clients.get(id)
    if (slot && (slot.child || slot.cleanup === 'unconfirmed')) throw new Error('请先关闭此档案窗口，等待清理完成后再删除。')
    remove()
    this.clients.delete(id)
  }

  /** Opening the same live profile requests focus, never another process.
   * @param {string} id */
  open(id) {
    const current = this.clients.get(id)
    if (current?.state === 'starting' || current?.state === 'active') { this.focus(id); return }
    if (current?.state === 'closing') throw new Error('此档案正在关闭，请等待模型服务清理完成。')
    if (current && (current.child || current.cleanup === 'unconfirmed')) throw new Error(`此档案清理未确认，保持锁定。${current.error}`)

    const home = id === 'existing' ? this.settings.root : path.join(this.settings.root, 'profiles', id)
    const logs = path.join(home, 'logs')
    const run = runName()
    const args = [...this.settings.entry, `--data=${this.settings.root}`, `--profile=${id}`, ...(this.settings.options ?? []), ...chromiumSwitches(logs, run), `--run=${run}`]
    const env = { ...process.env }
    delete env.ELECTRON_RUN_AS_NODE
    /** @type {Slot} */
    const slot = { id, state: 'starting', cleanup: 'unconfirmed', error: '', child: null }
    this.clients.set(id, slot)
    let child
    try {
      fs.mkdirSync(logs, { recursive: true })
      // This is the user's GUI client. STARTUPINFO's SW_HIDE overrides its
      // first ShowWindow call on Windows and otherwise forces a second Open.
      child = (this.settings.spawn ?? spawn)(this.settings.executable, args, { env, stdio: ['ignore', 'pipe', 'pipe', 'ipc'], windowsHide: false })
    } catch (cause) {
      slot.state = 'failed'; slot.cleanup = 'not-started'; slot.error = String(cause)
      this.emit('change')
      throw cause
    }
    slot.child = child
    let diagnostic = ''
    const owns = () => this.clients.get(id) === slot && slot.child === child
    const starting = setTimeout(() => {
      if (!owns() || slot.state !== 'starting') return
      slot.state = 'failed'; slot.error = '账号窗口未能在 45 秒内打开，等待该档案的清理确认。'
      this.emit('change'); child.kill()
    }, 45_000)
    starting.unref()
    child.stdout?.resume()
    child.stderr?.on('data', data => { diagnostic = (diagnostic + String(data)).slice(-2000) })
    child.on('message', message => {
      if (!owns() || !message || typeof message !== 'object' || !('type' in message)) return
      if (message.type === 'profile-show-chooser') { this.emit('show'); return }
      if (message.type === 'profile-closing') {
        clearTimeout(starting)
        if (slot.state !== 'failed') slot.state = 'closing'
      } else if (message.type === 'profile-released') {
        clearTimeout(starting); slot.cleanup = 'acknowledged'; slot.state = 'closing'
      } else if (message.type === 'profile-shutdown-failed') {
        clearTimeout(starting); slot.state = 'failed'; slot.cleanup = 'unconfirmed'
        slot.error = 'error' in message ? String(message.error) : '档案服务清理失败。'
      } else if (message.type === 'profile-ready' && slot.state === 'starting') {
        clearTimeout(starting); slot.state = 'active'; this.emit('ready', id)
      } else return
      this.emit('change')
    })
    child.on('error', error => {
      if (!owns()) return
      clearTimeout(starting); slot.error = error.message
      slot.state = slot.cleanup === 'acknowledged' ? 'closing' : 'failed'
      this.emit('change')
    })
    child.once('close', (code, signal) => {
      clearTimeout(starting)
      if (!owns()) return
      slot.child = null
      if (child.pid === undefined) slot.cleanup = 'not-started'
      if (slot.cleanup === 'unconfirmed') {
        slot.state = 'failed'
        slot.error = `未确认此档案的模型服务已释放，仅此档案保持锁定。${slot.error || diagnostic ? `\n${slot.error || diagnostic}` : ''}`
      } else if (slot.error || code !== 0) {
        slot.state = 'failed'
        slot.error ||= `档案窗口未正常关闭 (${signal ?? code})。${diagnostic ? `\n${diagnostic}` : ''}`
      } else this.clients.delete(id)
      this.emit('change')
      this.emit('closed', { id, error: slot.error })
    })
    this.emit('change')
  }

  /** @param {string} id */
  focus(id) {
    const slot = this.clients.get(id)
    if (slot?.child?.connected) slot.child.send({ type: 'focus' }, error => {
      if (error && this.clients.get(id) === slot && slot.child) { slot.error = error.message; this.emit('change') }
    })
  }
}
