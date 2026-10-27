import { useEffect, useRef, useState, type KeyboardEvent, type MouseEvent, type WheelEvent } from 'react'
import { gameInput, listen } from '@/lib/host'
import { cssCursor, keyAction, mods, toGame, wheelPixels } from '@/lib/gameInput'
import { tr } from '@/lib/i18n'

/** The element the shell's preload plays the game's frames in. */
export const GAME_VIDEO_ID = 'akagi-game-view'

/**
 * The game, drawn offscreen, as this page shows it: a video of its frames
 * filling the stage. The user's mouse and keyboard over it go to the game
 * (through the shell), and the cursor is the one the game asks for. A hidden
 * field holds the focus while the stage does, so that typed and composed text
 * arrives as text; keys that do something else go as keys.
 */
export function GameStage() {
  const box = useRef<HTMLDivElement>(null)
  const field = useRef<HTMLTextAreaElement>(null)
  const [cursor, setCursor] = useState('default')
  const [shown, setShown] = useState(false)
  useEffect(() => listen<string>('game-cursor', type => setCursor(cssCursor(type))), [])

  const at = (e: { clientX: number; clientY: number }) => {
    const r = box.current?.getBoundingClientRect()
    return r ? toGame(e.clientX, e.clientY, r) : { x: 0, y: 0 }
  }
  const MOUSE = { mousemove: 'move', mousedown: 'down', mouseup: 'up', mouseleave: 'leave' } as const
  const onMouse = (e: MouseEvent) => {
    const type = MOUSE[e.type as keyof typeof MOUSE]
    if (type) gameInput({ type, ...at(e), button: e.button, buttons: e.buttons, count: e.detail, mods: mods(e) })
  }
  const onDown = (e: MouseEvent) => {
    // The field takes the keyboard; the button stays the game's until it is let go.
    e.preventDefault()
    field.current?.focus({ preventScroll: true })
    box.current?.setPointerCapture?.((e.nativeEvent as PointerEvent).pointerId ?? 1)
    onMouse(e)
  }
  const onWheel = (e: WheelEvent) => {
    gameInput({ type: 'wheel', ...at(e), dx: wheelPixels(e.deltaX, e.deltaMode), dy: wheelPixels(e.deltaY, e.deltaMode), mods: mods(e) })
  }
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    const type = e.type === 'keyup' ? 'keyup' : 'keydown'
    const action = keyAction({ ...e, isComposing: e.nativeEvent.isComposing })
    if (!action) return
    e.preventDefault()
    if (action === 'paste') { if (type === 'keydown') gameInput({ type: 'paste' }) }
    else gameInput({ type, key: e.key, mods: mods(e) })
  }
  const send = (text: string | null | undefined) => {
    if (text) gameInput({ type: 'text', text })
    if (field.current) field.current.value = ''
  }

  return <div ref={box} className="mon-game" style={{ cursor }}
    onMouseMove={onMouse} onMouseDown={onDown} onMouseUp={onMouse} onMouseLeave={onMouse}
    onWheel={onWheel} onContextMenu={e => e.preventDefault()}>
    <video id={GAME_VIDEO_ID} className="mon-game-video" muted autoPlay playsInline disablePictureInPicture
      onPlaying={() => setShown(true)} onEmptied={() => setShown(false)} />
    {!shown && <p className="mon-game-wait">{tr('等待游戏画面')}</p>}
    <textarea ref={field} className="mon-game-keys" aria-label={tr('向游戏输入')} tabIndex={-1} spellCheck={false} autoComplete="off"
      onKeyDown={onKey} onKeyUp={onKey}
      onInput={e => { if (!(e.nativeEvent as InputEvent).isComposing) send(e.currentTarget.value) }}
      onCompositionEnd={e => send(e.currentTarget.value || e.data)} />
  </div>
}
