import { useEffect, useRef, useState } from 'react'
import { invoke, listen } from '@/lib/host'
import { tr } from '@/lib/i18n'

/** What the shell's mirror says (`desktop/src/mirror.js`): its state, and which picture to ask for while showing. */
export type MirrorState = { state: 'searching' | 'showing' | 'lost'; source: string | null }

/**
 * A native client's game, as this page shows it: the picture of the client's
 * own window, fitted to the stage, view only — nothing here goes to the game.
 * The shell owns the mirror's state; this asks for the picture while the
 * state is `showing`, and reports when a picture it played ended.
 */
export function MirrorStage({ name, onError }: { name: string; onError: (message: string) => void }) {
  const video = useRef<HTMLVideoElement>(null)
  const [mirror, setMirror] = useState<MirrorState | null>(null)
  useEffect(() => {
    let live = true
    const stop = listen<MirrorState>('game-mirror', next => { if (live) setMirror(next) })
    // After a reload of this page: the state the mirror is in, unless it has said so since.
    void invoke<MirrorState | null>('get_game_mirror').then(
      now => { if (live && now) setMirror(current => current ?? now) },
      e => { if (live) onError(String(e)) },
    )
    return () => { live = false; stop() }
  }, [onError])

  const source = mirror?.state === 'showing' ? mirror.source : null
  useEffect(() => {
    if (!source) return
    const element = video.current
    let live = true
    let stream: MediaStream | null = null
    navigator.mediaDevices.getDisplayMedia({ video: true, audio: false }).then(got => {
      if (!live) { got.getTracks().forEach(track => track.stop()); return }
      stream = got
      got.getVideoTracks()[0]?.addEventListener('ended', () => {
        if (live) void invoke('game_mirror_ended', { source }).catch(e => onError(String(e)))
      })
      if (element) { element.srcObject = got; void element.play().catch(() => {}) }
    }, e => { if (live) onError(String(e)) })
    return () => {
      live = false
      stream?.getTracks().forEach(track => track.stop())
      if (element) element.srcObject = null
    }
  }, [source, onError])

  // A line for the two states without a window to show; none while showing.
  const line = mirror?.state === 'showing' ? null
    : mirror?.state === 'lost' ? tr('{0} 窗口已关闭或最小化', name)
    : tr('等待 {0} 窗口', name)
  return <div className="mon-game" data-view="mirror">
    <video ref={video} className="mon-game-video mon-game-video--fit" muted autoPlay playsInline disablePictureInPicture />
    {line && <p className="mon-game-wait">{line}</p>}
  </div>
}
