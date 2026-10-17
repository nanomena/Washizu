// @ts-check
// The shell's end of the core's protocol (`src/headless`): start the core, send it
// requests, hand on its events. JSON lines over the child's stdin and stdout.
// No Electron here, so it is tested with plain Node against a stand-in core.

import { spawn } from 'node:child_process'
import { EventEmitter } from 'node:events'
import { createInterface } from 'node:readline'

/** How long the core has to say it is ready. A first run installs nothing by itself, so this is all start-up. */
const READY_TIMEOUT_MS = 30_000
/** How much of what the core wrote to stderr is kept to explain a failure. */
const STDERR_KEPT = 4_000

/**
 * @typedef {{ brand: string, monitor: { always_on_top: boolean, pointer_trail?: boolean, pointer_lean?: boolean, quick_confirm?: boolean, riichi_auto_win?: boolean, auto_continue?: boolean }, version: string }} Ready
 * @typedef {{ done: (value: unknown) => void, fail: (reason: Error) => void }} Waiting
 */

/**
 * A running core. Emits `event` (name, payload) for everything the core says
 * unasked, and `exit` (reason) once, when the process is gone.
 */
export class Core extends EventEmitter {
  /**
   * @param {string} command the core's executable
   * @param {string[]} args
   * @param {{ cwd?: string, env?: NodeJS.ProcessEnv }} [options]
   */
  constructor(command, args, options = {}) {
    super()
    /** @type {Map<number, Waiting>} */
    this.waiting = new Map()
    this.next = 0
    this.stderr = ''
    this.gone = false
    this.child = spawn(command, args, { ...options, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
    /** @type {Promise<Ready>} */
    this.ready = new Promise((done, fail) => {
      const timer = setTimeout(() => fail(new Error(`The core did not start within ${READY_TIMEOUT_MS / 1000} s.`)), READY_TIMEOUT_MS)
      this.once('ready', ready => { clearTimeout(timer); done(ready) })
      this.once('exit', reason => { clearTimeout(timer); fail(new Error(reason)) })
    })
    // A caller that never looks at `ready` still gets `exit`.
    this.ready.catch(() => {})

    createInterface({ input: this.child.stdout }).on('line', line => this.#read(line))
    this.child.stderr.setEncoding('utf8').on('data', text => {
      this.stderr = (this.stderr + text).slice(-STDERR_KEPT)
    })
    this.child.stdin.on('error', () => {})
    this.child.on('error', error => this.#end(`The core could not be started: ${error.message}`))
    // `close`, not `exit`: by then everything it wrote has been read.
    this.child.on('close', (code, signal) => this.#end(`The core stopped (${signal ?? `exit ${code}`}).`))
  }

  /**
   * Ask the core for something. Rejects with the core's own message, which is
   * written for the interface to show as is.
   * @param {string} method
   * @param {Record<string, unknown>} [params]
   * @returns {Promise<unknown>}
   */
  call(method, params = {}) {
    if (this.gone) return Promise.reject(new Error('The core is not running.'))
    const id = ++this.next
    return new Promise((done, fail) => {
      this.waiting.set(id, { done, fail })
      this.child.stdin.write(JSON.stringify({ id, method, params }) + '\n')
    })
  }

  /** Let the core go: it ends when its stdin closes, and takes the bot's processes with it. */
  close() {
    this.child.stdin.end()
  }

  /** @param {string} line */
  #read(line) {
    /** @type {any} */
    let message
    try { message = JSON.parse(line) } catch { return }
    if (!message || typeof message !== 'object') return
    if (typeof message.event === 'string') {
      if (message.event === 'ready') this.emit('ready', message.payload)
      else this.emit('event', message.event, message.payload)
      return
    }
    const waiting = this.waiting.get(message.id)
    if (!waiting) return
    this.waiting.delete(message.id)
    if ('error' in message) waiting.fail(new Error(String(message.error)))
    else waiting.done(message.result)
  }

  /** @param {string} reason */
  #end(reason) {
    if (this.gone) return
    this.gone = true
    for (const { fail } of this.waiting.values()) fail(new Error(reason))
    this.waiting.clear()
    this.emit('exit', reason)
  }
}
