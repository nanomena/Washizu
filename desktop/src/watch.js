// What the shell notes about the game's page, from what it can see without touching the game: the
// page's own console, whether its drawing context is still there, and how many frames it paints.
// The game once stopped dead at a change of scene, its WebGL context lost while the GPU process ran
// on, and nothing was left to say why; these lines are kept for the next time (see `journal.js`).

/** How often the page's drawing context is looked at. */
export const WATCH_MS = 10_000

/**
 * The page's drawing context, read once the game has made its own: before
 * that, asking the canvas for a context would make one for it, not as the game
 * would. `null` while there is none to read.
 */
export const GL_STATE = `(() => {
  if (!window.unityInstance) return null
  const canvas = document.getElementById('unity-canvas')
  const gl = canvas && (canvas.getContext('webgl2') || canvas.getContext('webgl'))
  return gl ? { lost: gl.isContextLost(), width: gl.drawingBufferWidth, height: gl.drawingBufferHeight } : null
})()`

/**
 * Whether a line of the page's console is worth keeping: its warnings and
 * errors, and the few words that mark where the game is — a scene's assets
 * unloaded as it changes scene, anything about WebGL, its context or memory.
 * @param {string} level @param {string} message
 */
export function worthKeeping(level, message) {
  return level === 'warning' || level === 'error'
    || /webgl|context|unloading .* unused assets|out of memory|memory access|abort|exception/i.test(message)
}

/** @typedef {{ lost: boolean, width: number, height: number }} GlState */

/** The changes worth a line in what the drawing context is, read after read. */
export class GlWatch {
  constructor() {
    /** @type {GlState | null} */
    this.last = null
  }

  /**
   * The context as read now; the line to note if it changed, else `null`.
   * @param {GlState | null} state
   */
  seen(state) {
    const last = this.last
    if (!state) return null
    this.last = state
    if (!last) return `game: WebGL context ${state.lost ? 'lost' : 'live'} (${state.width}×${state.height})`
    if (state.lost && !last.lost) return 'game: WebGL context lost'
    if (!state.lost && last.lost) return `game: WebGL context live again (${state.width}×${state.height})`
    return null
  }

  /** The page was loaded again: the next read starts over. */
  reset() {
    this.last = null
  }
}

/**
 * Frames painted, counted between looks, and the changes worth a line: a page
 * that paints far fewer than it should, and one that paints as it should again.
 */
export class FrameMeter {
  /** @param {number} fps the frame rate asked of the page @param {number} every the time between looks, ms */
  constructor(fps, every) {
    this.seconds = every / 1000
    this.expected = fps * this.seconds
    this.count = 0
    this.low = false
  }

  paint() {
    this.count += 1
  }

  /** The frames since the last look, and the line to note if the page's pace changed. */
  take() {
    const frames = this.count
    this.count = 0
    const low = frames < this.expected / 4
    const line = low === this.low ? null
      : low ? `game: only ${frames} frames painted in ${this.seconds} s, of ${this.expected} asked`
      : `game: painting again, ${frames} frames in ${this.seconds} s`
    this.low = low
    return { frames, line }
  }
}

/**
 * The application's processes in one line: each kind with its memory, the
 * biggest first, as a GPU or renderer trouble asks about.
 * @param {readonly { type: string, pid: number, memory: { workingSetSize: number } }[]} metrics
 */
export function processLine(metrics) {
  return [...metrics]
    .sort((a, b) => b.memory.workingSetSize - a.memory.workingSetSize)
    .map(m => `${m.type} ${m.pid} ${Math.round(m.memory.workingSetSize / 1024)} MB`)
    .join(', ')
}
