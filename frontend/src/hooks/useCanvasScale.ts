import { useCallback, useEffect, useState } from 'react'
import { CANVAS } from '@/lib/canvas'

/** Scale the complete fixed canvas; slots do not reflow with the window. */
export function useCanvasScale(): number {
  const fit = useCallback(() => Math.min(window.innerWidth / CANVAS.width, window.innerHeight / CANVAS.height) || 1, [])
  const [scale, setScale] = useState(fit)
  useEffect(() => {
    const apply = () => setScale(fit())
    apply()
    window.addEventListener('resize', apply)
    return () => window.removeEventListener('resize', apply)
  }, [fit])
  return scale
}
