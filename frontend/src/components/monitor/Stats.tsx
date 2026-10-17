import { useEffect, useMemo, useState, type CSSProperties } from 'react'
import { invoke } from '@/lib/host'
import { ROOMS, ROOM_NAME, figuresOf, roomOf, totalsOf, yakuName, type GameRecord, type Room } from '@/lib/history'
import { signed } from '@/lib/monitor'
import { tr } from '@/lib/i18n'

/** Rank points game by game, summed as they go: one line, and where zero is. */
function Spark({ points }: { points: number[] }) {
  const w = 120, h = 40
  if (points.length === 0) return <svg className="mstat-spark" viewBox={`0 0 ${w} ${h}`} aria-hidden />
  const all = [0, ...points]
  const lo = Math.min(...all), hi = Math.max(...all)
  const y = (v: number) => (hi === lo ? h / 2 : 2 + (h - 4) * (hi - v) / (hi - lo))
  const x = (i: number) => (all.length === 1 ? 0 : w * i / (all.length - 1))
  return <svg className="mstat-spark" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" role="img" aria-label={tr('段位分走势')}>
    <line x1={0} x2={w} y1={y(0)} y2={y(0)} />
    <polyline points={all.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ')} />
  </svg>
}

const share = (part: number, whole: number) => (whole > 0 ? `${(100 * part / whole).toFixed(1)}%` : '—')

/** Our record over the games on file, in three panels that keep their places
 *  whatever is in them: rank points and placement, the figures a riichi player
 *  looks for first, and the yaku our wins were made of. The last two scroll.
 *  It opens on the room we play in now, the latest game's; another range
 *  picked holds while the tab is open. `refresh` changes when the record may
 *  have: a game has started or ended. */
export function Statistics({ refresh }: { refresh: string }) {
  const [records, setRecords] = useState<GameRecord[] | null>(null)
  const [failed, setFailed] = useState<string | null>(null)
  const [picked, setPicked] = useState<Room | 'all' | null>(null)
  useEffect(() => {
    let live = true
    const read = () => invoke<GameRecord[]>('get_history').then(
      found => { if (live) { setRecords(found); setFailed(null) } },
      e => { if (live) setFailed(String(e)) },
    )
    void read()
    // A game that has just ended is written a moment after its last event.
    const again = setTimeout(() => void read(), 1500)
    return () => { live = false; clearTimeout(again) }
  }, [refresh])

  const rooms = useMemo(() => ROOMS.filter(key => (records ?? []).some(r => roomOf(r) === key)), [records])
  // The core lists the games oldest first.
  const latest = records?.at(-1)
  const room = picked ?? (latest ? roomOf(latest) : 'all')
  const shown = useMemo(() => (records ?? []).filter(r => room === 'all' || roomOf(r) === room), [records, room])
  const t = useMemo(() => totalsOf(shown), [shown])
  const figures = useMemo(() => figuresOf(t), [t])
  const waiting = failed ? tr('读不到对局记录：{0}', failed) : records == null ? tr('正在读取对局记录') : null

  return <div className="md-page mstats">
    <section className="mstat mstat-rank">
      <header className="msec-head">
        <h3>{tr('段位分与顺位')}</h3>
        <select className="mselect" aria-label={tr('统计范围')} value={room} onChange={e => setPicked(e.target.value as Room | 'all')}>
          <option value="all">{tr('全部场次')}</option>
          {rooms.map(key => <option key={key} value={key}>{ROOM_NAME[key]}</option>)}
        </select>
      </header>
      <div className="mstat-points">
        <strong>{t.pointGames > 0 ? signed(t.points.at(-1)!) : '—'}</strong>
        <small>{waiting ?? (t.pointGames > 0 ? tr('{0} 场的段位分变化', t.pointGames) : tr('还没有记录到段位分'))}</small>
        <Spark points={t.points} />
      </div>
      <ol className="mstat-places">
        {t.places.map((n, i) => <li key={i}>
          <span>{tr('{0} 位', i + 1)}</span>
          <i style={{ '--share': t.games > 0 ? n / t.games : 0 } as CSSProperties} />
          <b>{n}</b>
          <small>{share(n, t.games)}</small>
        </li>)}
      </ol>
      <footer><span>{tr('平均顺位')} <b>{t.averagePlace != null ? t.averagePlace.toFixed(2) : '—'}</b></span><span>{tr('{0} 场', t.games)}</span></footer>
    </section>

    <section className="mstat mstat-figures">
      <header className="msec-head"><h3>{tr('数据')}<span>{tr('{0} 局', t.hands)}</span></h3></header>
      <dl className="mstat-scroll">
        {figures.map(f => <div key={f.name} className="mfact" title={f.note}><dt>{f.name}</dt><dd>{f.value}</dd></div>)}
      </dl>
    </section>

    <section className="mstat mstat-yakus">
      <header className="msec-head"><h3>{tr('役种')}<span>{tr('{0} 次和了', t.scoredWins)}</span></h3></header>
      {t.yaku.length === 0 ? <p className="mslot-empty">{tr('役种从记录它的版本起开始累计')}</p>
        : <ol className="mstat-scroll">
          {t.yaku.map(y => <li key={y.id} className="mfact mstat-yaku"><span>{yakuName(y.id)}</span><b>{y.wins}</b><small>{share(y.wins, t.scoredWins)}</small></li>)}
        </ol>}
    </section>
  </div>
}
