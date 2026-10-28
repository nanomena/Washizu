// @ts-check
// Observe the physical cursor; the core owns manual input and its hand-back.
import { handOnGame, onGame } from './layout.js'

/** @param {Buffer} handle */
export function nativeWindowIdentity(handle) {
  if (handle.length !== 4 && handle.length !== 8) throw new Error('Unsupported native window handle')
  return (handle.length === 8 ? handle.readBigUInt64LE() : BigInt(handle.readUInt32LE())).toString(16)
}

export class PhysicalPointerObserver {
  /** @param {(params: Record<string, unknown>) => Promise<unknown>} send
   * @param {(error: unknown) => void} failed
   * @param {number} [stillMs] */
  constructor(send, failed, stillMs = 150) {
    this.send = send
    this.failed = failed
    this.stillMs = stillMs
    /** @type {{x:number,y:number} | null} */ this.point = null
    this.movedAt = Number.NEGATIVE_INFINITY
    this.active = false
    this.overGame = false
    this.pending = false
    this.window = ''
  }

  /** @param {{point:{x:number,y:number},area:import('./layout.js').Rect|null,page:import('./layout.js').Size|null,window:string,now:number}} sample */
  observe(sample) {
    const { point, area, page, window, now } = sample
    if (window !== this.window) {
      this.window = window; this.point = null; this.active = false; this.overGame = false
    }
    if (!this.point) {
      this.point = point; this.movedAt = Number.NEGATIVE_INFINITY
    } else if (point.x !== this.point.x || point.y !== this.point.y) {
      this.point = point; this.movedAt = now
    }
    const active = handOnGame(point, area, this.movedAt, now, this.stillMs)
    const position = onGame(point, area, page)
    if (this.pending || (!active && active === this.active && !position && !this.overGame)) return
    // Resample inside a candidate game rectangle even while stationary: an
    // overlay can appear without moving or activating the underlying window.
    const at = active ? null : position
    const report = at ? { window, active, x: at.x, y: at.y } : { window, active }
    this.pending = true
    this.send(report)
      .then(() => { if (this.window === window) { this.active = active; this.overGame = position !== null } })
      .catch(this.failed)
      .finally(() => { this.pending = false })
  }
}
