import { useLayoutEffect, useRef } from 'react'
import { AlertTriangle, Check, ChevronRight } from 'lucide-react'
import { Faces } from '@/components/monitor/Tiles'
import { SeatTag } from '@/components/monitor/Table'
import { eventLine, seatLabel, tableOf, type Alert, type MonitorEvent, type MonitorSnapshot } from '@/lib/monitor'
import { tr } from '@/lib/i18n'

const clock = (ms: number) => new Date(ms).toLocaleTimeString('zh-CN', { hour12: false })

/** One hand's public events, read top to bottom in the order they happened.
 *  The newest line is kept in view unless the reader has scrolled back to look
 *  at an earlier one. `events` is `null` while an earlier hand's are on their
 *  way; `live` for the hand in play, whose confirmed move is marked. */
export function HandEvents({ snapshot: s, events, live }: { snapshot: MonitorSnapshot; events: MonitorEvent[] | null; live: boolean }) {
  const table = tableOf(s.game)
  const confirmed = s.decision?.confirmed_seq ?? null
  const list = useRef<HTMLOListElement>(null)
  const following = useRef(true)
  const newest = events?.at(-1)?.seq
  useLayoutEffect(() => {
    const el = list.current
    if (el && following.current) el.scrollTop = el.scrollHeight
  }, [newest])
  if (events == null) return <p className="mslot-empty">{tr('正在读取这一局的事件')}</p>
  // Another seat's draw is face down: a line that says nothing, twice a turn.
  const shown = events.filter(e => !(e.event.type === 'tsumo' && e.event.pai === '?'))
  if (shown.length === 0) return <p className="mslot-empty">{tr('这一局还没有事件')}</p>
  return <ol className="mevents" ref={list} onScroll={e => { const el = e.currentTarget; following.current = el.scrollHeight - el.scrollTop - el.clientHeight < 12 }}>
    {shown.map(e => {
      const line = eventLine(e.event, table)
      return <li key={e.seq} className="mevent" data-quiet={line.quiet || undefined} data-self={(line.seat != null && line.seat === table.self) || undefined} data-system={line.seat == null || undefined}>
        <time>{clock(e.ts_ms)}</time>
        <span className="mevent-seat">{line.seat != null && <><SeatTag seat={line.seat} table={table} />{seatLabel(line.seat, table)}<small>{s.names[line.seat] ?? ''}</small></>}</span>
        <span className="mevent-text">{line.text}</span>
        <span className="mevent-faces"><Faces faces={line.faces} /></span>
        <span className="mevent-mark">{live && e.seq === confirmed && <span className="mevent-ok"><Check size={11} strokeWidth={3} />{tr('与建议一致')}</span>}</span>
      </li>
    })}
  </ol>
}

/** Why there is no valid suggestion right now, in the slot where the candidates
 *  would otherwise be. */
export function FaultCard({ alerts, onDetails }: { alerts: Alert[]; onDetails?: () => void }) {
  return <section className="mfault" role="alert" data-count={Math.min(alerts.length, 3)}>
    <ul>
      {alerts.slice(0, 3).map(a => <li key={a.key}>
        <strong><AlertTriangle size={14} />{a.title}</strong>
        <p title={a.message}>{a.message}</p>
      </li>)}
    </ul>
    <footer>
      <small>{alerts.length > 3 ? tr('另有 {0} 项', alerts.length - 3) : alerts[0].hint}</small>
      {onDetails && <button onClick={onDetails}>{tr('连接详情')}<ChevronRight size={12} /></button>}
    </footer>
  </section>
}
