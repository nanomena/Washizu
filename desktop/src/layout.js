// @ts-check
// The window's geometry. Pure: no Electron here, so it is tested with plain Node.

/**
 * @typedef {{ x: number, y: number, width: number, height: number }} Rect
 * @typedef {{ width: number, height: number }} Size
 */

/**
 * The window is a fixed canvas the page scales as one piece: the game at 16:9
 * with a quarter-height dock under it, 4:3 in all. It keeps that ratio at every
 * size — its floor, its default and whatever it is dragged to.
 */
export const SIZE = Object.freeze({ width: 1200, height: 900 })
export const MIN = Object.freeze({ width: 880, height: 660 })
export const RATIO = SIZE.width / SIZE.height

/**
 * The size a fresh window opens at: its own, or as much of it as the display
 * has room for.
 * @param {Size} workArea
 * @returns {Size}
 */
export function openingSize(workArea) {
  const fit = Math.min(workArea.width * 0.92 / SIZE.width, workArea.height * 0.84 / SIZE.height, 1)
  return { width: Math.max(Math.round(SIZE.width * fit), MIN.width), height: Math.max(Math.round(SIZE.height * fit), MIN.height) }
}

/**
 * A content size brought back to the canvas's ratio, keeping its width.
 * @param {Size} size
 * @returns {Size}
 */
export function conform(size) {
  const width = Math.max(Math.round(size.width), MIN.width)
  return { width, height: Math.round(width / RATIO) }
}

/**
 * The content size a resize comes to rest at: on the ratio, following whichever
 * of the two dimensions the drag changed more.
 * @param {Size} before the content size when the drag began
 * @param {Size} after where the drag left it
 * @returns {Size}
 */
export function settle(before, after) {
  const wider = Math.abs(after.width - before.width)
  const taller = Math.abs(after.height - before.height)
  return conform({ width: taller * RATIO > wider ? after.height * RATIO : after.width, height: 0 })
}

/**
 * The ratio of a whole window — borders and title bar included — whose content
 * is on the canvas's ratio at `contentWidth`. For a platform that can only
 * hold the ratio of the whole window.
 * @param {number} contentWidth
 * @param {Size} border what the window is larger than its content by
 * @returns {number}
 */
export function framedRatio(contentWidth, border) {
  return (contentWidth + border.width) / (contentWidth / RATIO + border.height)
}

/**
 * Where the window was last left, if that is still somewhere it can be seen
 * and used: all of it inside one display's work area.
 * @param {unknown} saved
 * @param {Rect[]} workAreas
 * @returns {Rect | null}
 */
export function restoredBounds(saved, workAreas) {
  if (!saved || typeof saved !== 'object') return null
  const { x, y, width, height } = /** @type {Record<string, unknown>} */ (saved)
  if (![x, y, width, height].every(v => typeof v === 'number' && Number.isFinite(v))) return null
  const size = conform({ width: /** @type {number} */ (width), height: /** @type {number} */ (height) })
  const bounds = { x: Math.round(/** @type {number} */ (x)), y: Math.round(/** @type {number} */ (y)), ...size }
  const inside = workAreas.some(area => bounds.x >= area.x && bounds.y >= area.y
    && bounds.x + bounds.width <= area.x + area.width && bounds.y + bounds.height <= area.y + area.height)
  return inside ? bounds : null
}

/**
 * Where the pointer's trail goes on screen: exactly over the game view, which
 * sits at `game` in the window's content.
 * @param {Rect} content the window's content area, on screen
 * @param {Rect} game the game view, in the content
 * @returns {Rect}
 */
export function trailBounds(content, game) {
  return { x: content.x + game.x, y: content.y + game.y, width: game.width, height: game.height }
}

/** Only the focused account can attribute the physical cursor to its game.
 * Overlapping background windows must not report another window's manual input.
 * @param {Rect} content
 * @param {Rect | null} game
 * @param {boolean} focused
 * @param {boolean} minimized
 * @returns {Rect | null}
 */
export function manualPointerArea(content, game, focused, minimized) {
  return focused && !minimized && game ? trailBounds(content, game) : null
}

/**
 * Whether the user's own mouse is at work over the game: the system cursor
 * is inside the game view and moved within the last `stillMs`. Input sent
 * over DevTools never moves the system cursor, so this is the user alone.
 * @param {{ x: number, y: number }} point the system cursor, on screen
 * @param {Rect | null} area the game view, on screen
 * @param {number} movedAt when the cursor last moved, in ms
 * @param {number} now
 * @param {number} stillMs
 * @returns {boolean}
 */
export function handOnGame(point, area, movedAt, now, stillMs) {
  if (!area) return false
  const inside = point.x >= area.x && point.x < area.x + area.width && point.y >= area.y && point.y < area.y + area.height
  return inside && now - movedAt < stillMs
}

/**
 * Where on the game the system cursor is, in the game page's CSS pixels;
 * `null` off the game. The view on screen is the page at its own size, unless
 * the page is rendered offscreen at a size of its own (`page`) and the view
 * is the monitor's picture of it.
 * @param {{ x: number, y: number }} point the system cursor, on screen
 * @param {Rect | null} area the game view, on screen
 * @param {Size | null} [page] the page's size, when it is not the view's
 * @returns {{ x: number, y: number } | null}
 */
export function onGame(point, area, page = null) {
  if (!area) return null
  const inside = point.x >= area.x && point.x < area.x + area.width && point.y >= area.y && point.y < area.y + area.height
  if (!inside) return null
  const sx = page ? page.width / area.width : 1
  const sy = page ? page.height / area.height : 1
  return { x: (point.x - area.x) * sx, y: (point.y - area.y) * sy }
}

/**
 * A point on the game page, in the view that shows it: where the pointer's
 * trail draws it. The inverse of `onGame`'s scaling, without the offset (the
 * trail lies over the view).
 * @template {{ x: number, y: number }} T
 * @param {T} mark in the page's CSS pixels
 * @param {Size} view the view's size
 * @param {Size | null} page the page's size, when it is not the view's
 * @returns {T}
 */
export function onView(mark, view, page) {
  if (!page) return mark
  return { ...mark, x: mark.x * view.width / page.width, y: mark.y * view.height / page.height }
}

/**
 * Where the page wants the game view. The page measures its stage element and
 * says so; nothing it says is trusted further than this.
 * @param {unknown} value
 * @returns {Rect & { visible: boolean }}
 */
export function gameBounds(value) {
  const bounds = /** @type {Record<string, unknown>} */ (value && typeof value === 'object' ? value : {})
  const { x, y, width, height } = bounds
  const numbers = [x, y, width, height]
  if (!numbers.every(v => typeof v === 'number' && Number.isFinite(v))) throw new Error('Invalid game view bounds.')
  const [left, top, wide, tall] = /** @type {number[]} */ (numbers)
  if (left < 0 || top < 0 || wide < 100 || tall < 100 || wide > 16000 || tall > 16000) throw new Error('Invalid game view bounds.')
  // Views sit on whole pixels; a fraction would leave a seam beside the dock.
  return { x: Math.round(left), y: Math.round(top), width: Math.round(wide), height: Math.round(tall), visible: bounds.visible !== false }
}
