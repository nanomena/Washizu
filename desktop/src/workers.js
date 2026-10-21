// @ts-check
// Service workers, let run as each one starts.
//
// Since client 4.0.51 the game starts through a service worker of its own: its
// page waits for the worker before it loads anything, and the worker answers
// the game's requests from then on. The core's DevTools client attaches to the
// workers of the pages it watches as they start and has them wait for it
// (`waitForDebuggerOnStart`); a service worker it then tells to run and leaves
// at once. That worker stays "starting" all the same, and so does whatever
// waits on it: the page stays black at its first start, and a request it makes
// later, after Chromium stopped the idle worker and starts it again, is never
// answered. A worker told to run by a session that waits for the answer does
// run. Here the shell watches the browser's targets over its DevTools port and
// does that for each service worker as it appears.

/** How long to wait for the DevTools port to answer at start, and how often to ask. */
const CONNECT_TRIES = 20
const CONNECT_EVERY_MS = 500

/**
 * The DevTools messages that let every service worker run: the browser's
 * messages go to `receive`, and those to send come out of `send`.
 */
export class WorkerRelease {
  #send
  #onRelease
  #id = 0
  /** What to do with each answer still to come, by its command's id. */
  /** @type {Map<number, (result: any) => void>} */
  #pending = new Map()
  /** The workers seen, so a worker is let run once however often it is reported. */
  /** @type {Set<string>} */
  #seen = new Set()

  /**
   * @param {(message: object) => void} send
   * @param {(url: string) => void} [onRelease] a worker was told to run and let go of
   */
  constructor(send, onRelease = () => {}) {
    this.#send = send
    this.#onRelease = onRelease
  }

  /** The first message: report every target, those there now and those to come. */
  start() {
    this.#call('Target.setDiscoverTargets', { discover: true })
  }

  /** @param {any} message one message from the browser */
  receive(message) {
    if (typeof message?.id === 'number') {
      const then = this.#pending.get(message.id)
      this.#pending.delete(message.id)
      then?.(message.result)
      return
    }
    if (message?.method === 'Target.targetCreated' || message?.method === 'Target.targetInfoChanged') {
      const info = message.params?.targetInfo
      if (info?.type === 'service_worker' && !this.#seen.has(info.targetId)) {
        this.#seen.add(info.targetId)
        this.#release(info.targetId, String(info.url ?? ''))
      }
    }
    if (message?.method === 'Target.targetDestroyed') this.#seen.delete(message.params?.targetId)
  }

  /** @param {string} targetId @param {string} url */
  #release(targetId, url) {
    this.#call('Target.attachToTarget', { targetId, flatten: true }, result => {
      const sessionId = result?.sessionId
      if (!sessionId) return
      // Let go only once the worker has answered: told to run and left at once, it stays put.
      this.#call('Runtime.runIfWaitingForDebugger', {}, () => {
        this.#call('Target.detachFromTarget', { sessionId })
        this.#onRelease(url)
      }, sessionId)
    })
  }

  /**
   * @param {string} method @param {object} params
   * @param {(result: any) => void} [then] @param {string} [sessionId]
   */
  #call(method, params, then, sessionId) {
    const id = ++this.#id
    if (then) this.#pending.set(id, then)
    this.#send(sessionId ? { id, method, params, sessionId } : { id, method, params })
  }
}

/**
 * Let every service worker of the browser whose DevTools listen on `port` run
 * as it starts, for as long as the browser runs.
 * @param {number} port @param {(message: string) => void} log
 */
export async function keepWorkersRunning(port, log) {
  let address = ''
  for (let tries = 0; !address && tries < CONNECT_TRIES; tries++) {
    try {
      const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json()
      address = String(version.webSocketDebuggerUrl ?? '')
    } catch {
      await new Promise(done => setTimeout(done, CONNECT_EVERY_MS))
    }
  }
  if (!address) {
    log('workers: the DevTools port did not answer; service workers are left as they start')
    return
  }
  const socket = new WebSocket(address)
  let told = false
  const release = new WorkerRelease(message => socket.send(JSON.stringify(message)), url => {
    // A worker starts again whenever it was stopped for being idle: said once.
    if (told) return
    told = true
    log(`workers: ${url.split('?')[0]} let run as it started, and from now on at each start`)
  })
  socket.addEventListener('open', () => release.start())
  socket.addEventListener('message', event => {
    try { release.receive(JSON.parse(String(event.data))) } catch {}
  })
  socket.addEventListener('close', () => log('workers: the DevTools connection closed'))
}
