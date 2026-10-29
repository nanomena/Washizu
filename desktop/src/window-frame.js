// @ts-check
// Native window presentation and game content ratio helpers.
import { RATIO, framedRatio, settle } from './layout.js'

/** Explicitly raise the account as well as requesting focus. A focus request
 * alone can leave a visible window behind the chooser on Windows.
 * @param {import('electron').BaseWindow | null} frame */
export function presentWindow(frame) {
  if (!frame) return
  if (frame.isMinimized()) frame.restore()
  frame.show()
  frame.moveTop()
  frame.focus()
}

/** @param {import('electron').BaseWindow} frame */
export function contentSize(frame) {
  const [width, height] = frame.getContentSize()
  return { width, height }
}

/**
 * Keep the window's content on the canvas's ratio while an edge is dragged.
 * macOS and X11 hold a content ratio themselves. Windows holds the ratio of the
 * whole window, title bar included, so there the ratio asked for is the one
 * that puts the content right at its present width, said again as that changes.
 * Wherever a drag ends, the content is then put exactly on the ratio.
 * @param {import('electron').BaseWindow} frame
 */
export function holdRatio(frame) {
  if (process.platform === 'win32') {
    const [outerWidth, outerHeight] = frame.getSize()
    const inner = contentSize(frame)
    const border = { width: outerWidth - inner.width, height: outerHeight - inner.height }
    const say = () => frame.setAspectRatio(framedRatio(contentSize(frame).width, border))
    say()
    frame.on('resize', say)
  } else {
    frame.setAspectRatio(RATIO)
  }

  let before = contentSize(frame)
  const rest = () => {
    const now = contentSize(frame)
    const exact = settle(before, now)
    if (Math.abs(exact.width - now.width) > 1 || Math.abs(exact.height - now.height) > 1) frame.setContentSize(exact.width, exact.height)
    before = exact
  }
  if (process.platform === 'linux') {
    // No event says a drag has ended here, and Wayland holds no ratio at all:
    // the size is at rest once it has stopped changing.
    /** @type {NodeJS.Timeout | undefined} */
    let quiet
    frame.on('resize', () => { clearTimeout(quiet); quiet = setTimeout(rest, 250) })
  } else {
    frame.on('resized', rest)
  }
}
