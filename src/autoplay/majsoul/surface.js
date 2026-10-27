// The current official client is Unity. Its hand is not a Laya JavaScript
// object: hand/window identity comes from the confirmed game protocol.
// Read only the actual Unity surface, including the final hit point.
(point => {
  const canvas = document.getElementById('unity-canvas');
  if (!(canvas instanceof HTMLCanvasElement)) return { view: null };
  const r = canvas.getBoundingClientRect(), style = getComputedStyle(canvas);
  if (!(canvas.width > 0 && canvas.height > 0 && r.width > 0 && r.height > 0)
      || style.display === 'none' || style.visibility !== 'visible') return { view: null };
  const p = point ?? [r.x + r.width / 2, r.y + r.height / 2];
  return { view: {
    viewport: { x: r.x, y: r.y, width: r.width, height: r.height },
    clear: document.elementFromPoint(p[0], p[1]) === canvas,
  } };
})
