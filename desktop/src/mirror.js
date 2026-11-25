// @ts-check
// The game area of a native client's account: a picture of the client's own
// window, view only. This file owns the mirror's state, and only this file
// changes it. No Electron here: the window list and the timer are handed in,
// so it is tested with plain Node.
//
//   searching --window found--> showing --window gone or its picture ended--> lost
//   lost --window found--> showing        lost --still none--> searching
//
// The dock plays the picture. It learns the state from this owner, asks for the
// picture while the state is `showing`, and reports when a picture it played
// ended; it decides nothing.

/** How often the window list is read while the mirror runs. */
export const MIRROR_POLL_MS = 2000

/**
 * @typedef {{ id: string, name: string }} WindowSource a window `desktopCapturer` lists
 * @typedef {{ state: 'searching' | 'lost', source: null } | { state: 'showing', source: WindowSource }} MirrorState
 * @typedef {{ type: 'listed', source: WindowSource | null } | { type: 'ended', id: string }} MirrorObservation
 */

/** @type {MirrorState} */
export const SEARCHING = Object.freeze({ state: 'searching', source: null })

/**
 * The game's window among the listed ones: the first whose title is one of the
 * client's (`PLATFORMS`), exactly. Nothing else is guessed at.
 * @param {readonly WindowSource[]} sources @param {readonly string[]} titles
 * @returns {WindowSource | null}
 */
export function pickSource(sources, titles) {
  return sources.find(source => titles.includes(source.name.trim())) ?? null
}

/**
 * The one transition function. A report about a picture other than the one
 * shown is stale and changes nothing.
 * @param {MirrorState} current @param {MirrorObservation} seen @returns {MirrorState}
 */
export function nextMirror(current, seen) {
  if (seen.type === 'ended') {
    return current.state === 'showing' && current.source.id === seen.id ? { state: 'lost', source: null } : current
  }
  const found = seen.source
  if (current.state === 'showing') {
    if (!found) return { state: 'lost', source: null }
    return found.id === current.source.id ? current : { state: 'showing', source: found }
  }
  if (found) return { state: 'showing', source: found }
  return current.state === 'lost' ? SEARCHING : current
}

/** What the dock is told: the state, and which picture to ask for while showing.
 * @param {MirrorState} mirror */
export function mirrorPayload(mirror) {
  return { state: mirror.state, source: mirror.source?.id ?? null }
}

/**
 * Runs the mirror: reads the window list every `MIRROR_POLL_MS` from `start`
 * until `stop`, and says every change of state.
 */
export class MirrorWatch {
  /**
   * @param {{ list: () => Promise<WindowSource[]>, titles: readonly string[], onState: (mirror: MirrorState) => void,
   *   log: (line: string) => void, every?: number,
   *   setTimer?: (run: () => void, ms: number) => unknown, clearTimer?: (timer: any) => void }} options
   */
  constructor({ list, titles, onState, log, every = MIRROR_POLL_MS, setTimer = setTimeout, clearTimer = clearTimeout }) {
    this.list = list
    this.titles = titles
    this.onState = onState
    this.log = log
    this.every = every
    this.setTimer = setTimer
    this.clearTimer = clearTimer
    /** @type {MirrorState} */
    this.mirror = SEARCHING
    this.running = false
    /** @type {unknown} */
    this.timer = null
    /** Whether the last read of the window list failed; noted once, not every read. */
    this.failing = false
  }

  /** @param {MirrorObservation} seen */
  #see(seen) {
    const next = nextMirror(this.mirror, seen)
    if (next === this.mirror) return
    this.mirror = next
    this.log(`mirror: ${next.state}${next.source ? ` (${next.source.id})` : ''}`)
    this.onState(next)
  }

  start() {
    if (this.running) return
    this.running = true
    this.onState(this.mirror)
    void this.#look()
  }

  stop() {
    this.running = false
    this.clearTimer(this.timer)
    this.timer = null
  }

  async #look() {
    if (!this.running) return
    try {
      const sources = await this.list()
      if (this.failing) this.log('mirror: the window list can be read again')
      this.failing = false
      if (this.running) this.#see({ type: 'listed', source: pickSource(sources, this.titles) })
    } catch (error) {
      // The state stays what it was seen to be; the next read decides.
      if (!this.failing) this.log(`mirror: the window list could not be read: ${error instanceof Error ? error.message : error}`)
      this.failing = true
    }
    if (this.running) this.timer = this.setTimer(() => void this.#look(), this.every)
  }

  /** The dock's report that the picture of `id` ended. @param {unknown} id */
  ended(id) {
    if (typeof id === 'string') this.#see({ type: 'ended', id })
  }

  /** The window to answer the dock's request for the picture with, while showing. */
  granted() {
    return this.running && this.mirror.state === 'showing' ? this.mirror.source : null
  }
}
