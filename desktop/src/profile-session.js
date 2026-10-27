// @ts-check
// One account process at a time. Only the actual process close releases the
// slot, so switching never overlaps old capture, core or model connections.
import { EventEmitter } from 'node:events'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { chromiumSwitches, runName } from './journal.js'

export class ProfileSession extends EventEmitter {
  /** @param {{ executable: string, entry: string[], root: string, options?: string[], spawn?: typeof spawn }} settings */
  constructor(settings) {
    super()
    this.settings = settings
    /** @type {import('node:child_process').ChildProcess | null} */
    this.child = null
    /** @type {string | null} */
    this.active = null
    this.ready = false
  }

  /** @param {string} id */
  open(id) {
    if (this.active !== null) throw new Error('请先从当前窗口返回账号档案；若服务释放失败，请检查错误并完成清理后再切换账号。')
    const home = id === 'existing' ? this.settings.root : path.join(this.settings.root, 'profiles', id)
    const logs = path.join(home, 'logs')
    fs.mkdirSync(logs, { recursive: true })
    const run = runName()
    const args = [...this.settings.entry, `--data=${this.settings.root}`, `--profile=${id}`, ...(this.settings.options ?? []), ...chromiumSwitches(logs, run), `--run=${run}`]
    const env = { ...process.env }
    delete env.ELECTRON_RUN_AS_NODE
    const child = (this.settings.spawn ?? spawn)(this.settings.executable, args, { env, stdio: ['ignore', 'pipe', 'pipe', 'ipc'], windowsHide: true })
    this.child = child
    this.active = id
    this.ready = false
    let diagnostic = ''
    let failure = ''
    let released = false
    const starting = setTimeout(() => { failure = '账号窗口未能在 45 秒内打开。'; child.kill() }, 45_000)
    child.stdout?.resume()
    child.stderr?.on('data', data => { diagnostic = (diagnostic + String(data)).slice(-2000) })
    child.on('message', message => {
      if (message && typeof message === 'object' && 'type' in message && message.type === 'profile-released') released = true
      if (message && typeof message === 'object' && 'type' in message && message.type === 'profile-ready') {
        clearTimeout(starting)
        this.ready = true
        this.emit('ready', id)
        this.emit('change')
      }
    })
    child.on('error', error => { failure = error.message })
    child.once('close', (code, signal) => {
      clearTimeout(starting)
      const uncertain = !released && child.pid !== undefined
      const error = uncertain ? `未确认旧账号的模型服务已释放，账号切换保持锁定。${failure || diagnostic ? `\n${failure || diagnostic}` : ''}` : failure || (code !== 0 ? `档案窗口未正常关闭 (${signal ?? code})。${diagnostic ? `\n${diagnostic}` : ''}` : '')
      this.child = null
      if (!uncertain) this.active = null
      this.ready = false
      this.emit('change')
      this.emit('closed', { id, error })
    })
    this.emit('change')
  }

  focus() {
    if (this.child?.connected) this.child.send({ type: 'focus' })
  }
}
