// @ts-check
// The guide fits account controls, not a game's 4:3 canvas. All sizes are DIP.
export const GUIDE = Object.freeze({
  width: 640,
  padding: 24,
  toolbar: 48,
  status: 24,
  tableHeader: 32,
  row: 64,
  footer: 24,
  minRows: 2, // Also fits the profile editor without resizing the guide.
  maxRows: 4,
})

/** @param {number} count @param {{width: number, height: number}} workArea */
export function guideSize(count, workArea) {
  const rows = Math.max(GUIDE.minRows, Math.min(GUIDE.maxRows, count))
  const height = GUIDE.padding * 2 + GUIDE.toolbar + GUIDE.status + GUIDE.tableHeader + rows * GUIDE.row + GUIDE.footer
  // Leave room for the native frame and space around this floating guide.
  return { width: Math.min(GUIDE.width, workArea.width - 48), height: Math.min(height, workArea.height - 80) }
}
