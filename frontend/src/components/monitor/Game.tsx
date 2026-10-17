import { useLayoutEffect, useRef, useState } from 'react'
import { HandEvents } from '@/components/monitor/Details'
import { Delta, ResultScores, RevealedHands, SeatTag } from '@/components/monitor/Table'
import { invoke } from '@/lib/host'
import {
  WIND, handLabel, handOutcome, resultTitle, seatLabel, tableOf,
  type HandSummary, type MonitorEvent, type MonitorSnapshot, type Table,
} from '@/lib/monitor'
import { tr } from '@/lib/i18n'

type View = 'result' | 'events'

/** A hand in the game's list, in three places of its own: which hand, how it
 *  ended (who won, or that nobody did), and what it did to our score. */
function HandLine({ hand, table, live, picked, onPick }: { hand: HandSummary; table: Table; live: boolean; picked: boolean; onPick: () => void }) {
  const r = hand.result
  const delta = table.self != null ? r?.deltas?.[table.self] : undefined
  return <li className="mgame-hand">
    <button aria-pressed={picked} onClick={onPick} title={handLabel(hand)}>
      <span className="mgame-hand-name"><strong>{tr('{0}{1}局', WIND[hand.bakaze] ?? hand.bakaze, hand.kyoku)}</strong>{hand.honba > 0 && <small>{tr('{0} 本场', hand.honba)}</small>}</span>
      <span className="mgame-hand-end" data-live={!r || undefined}>
        {!r ? <><i className="mdot" data-tone={live ? 'busy' : 'idle'} />{tr('进行中')}</>
          : r.kind !== 'hora' ? <span className="mgame-hand-draw">{tr('流局')}</span>
          : r.hands.length > 0 ? r.hands.map(h => <span key={h.seat} title={`${seatLabel(h.seat, table)} · ${handOutcome(h, table)}`}><SeatTag seat={h.seat} table={table} /></span>)
          : resultTitle(r)}
      </span>
      {r && <span className="mgame-hand-delta">{delta != null && <Delta value={delta} />}</span>}
    </button>
  </li>
}

/** The game, a hand at a time: every hand dealt so far down the left, the one
 *  picked on the right — how it ended, or how it went. The hand in play is
 *  picked until another is; a new hand dealt does not take the place of one
 *  picked. A hand still in play has only its events to show. */
export function GamePages({ snapshot: s }: { snapshot: MonitorSnapshot }) {
  const table = tableOf(s.game)
  const hands = s.hands
  // The hand picked, by its round; null follows the hand in play.
  const [picked, setPicked] = useState<number | null>(null)
  const [view, setView] = useState<View>('result')
  // An earlier hand's events, once asked for; `null` while they are on their way.
  const [fetched, setFetched] = useState<Record<number, MonitorEvent[] | null>>({})
  const list = useRef<HTMLOListElement>(null)
  const count = hands.length
  useLayoutEffect(() => {
    const el = list.current
    if (el && picked == null) el.scrollTop = el.scrollHeight
  }, [count, picked])
  if (count === 0) return <p className="mslot-empty md-wide">{tr('对局开始后，这里按局列出这一场每一局的结局和过程')}</p>

  const last = count - 1
  const found = picked == null ? -1 : hands.findIndex(h => h.round === picked)
  const at = found < 0 ? last : found
  const hand = hands[at]
  const pick = (h: HandSummary, i: number) => {
    setPicked(i === last ? null : h.round)
    if (i === last || h.round in fetched) return
    setFetched(f => ({ ...f, [h.round]: null }))
    invoke<MonitorEvent[]>('get_hand_events', { round: h.round }).then(
      events => setFetched(f => ({ ...f, [h.round]: events })),
      () => setFetched(f => { const rest = { ...f }; delete rest[h.round]; return rest }),
    )
  }
  const shown: View = view === 'result' && hand.result ? 'result' : 'events'
  // Whether the hand is the one whose table is still dealt: its melds can be read from it.
  const onTable = at === last && !!s.result

  return <div className="md-page mgame">
    <ol className="mgame-hands" ref={list} aria-label={tr('各局')}>
      {hands.map((h, i) => <HandLine key={h.round} hand={h} table={table} live={i === last && !s.game_ended} picked={i === at} onPick={() => pick(h, i)} />)}
    </ol>
    <section className="mgame-detail">
      <header className="msec-head">
        <h3>{handLabel(hand)}{hand.result ? <span className="mtag" data-tone={hand.result.kind === 'hora' ? 'win' : 'idle'}>{resultTitle(hand.result)}</span>
          : <span className="mtag" data-tone="ok">{tr('进行中')}</span>}</h3>
        <span className="mseg" role="group" aria-label={tr('这一局')}>
          <button aria-pressed={shown === 'result'} disabled={!hand.result} onClick={() => setView('result')}>{tr('结局')}</button>
          <button aria-pressed={shown === 'events'} onClick={() => setView('events')}>{tr('过程')}</button>
        </span>
      </header>
      {shown === 'result' && hand.result
        ? <div className="mgame-result">
          <RevealedHands result={hand.result} snapshot={s} previous={!onTable} max={26} />
          <ResultScores result={hand.result} snapshot={s} row />
        </div>
        : <HandEvents key={hand.round} snapshot={s} events={at === last ? s.events : fetched[hand.round] ?? null} live={at === last} />}
    </section>
  </div>
}
