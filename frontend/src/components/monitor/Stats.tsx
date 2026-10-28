import { useEffect, useMemo, useState, type CSSProperties } from 'react'
import { invoke } from '@/lib/host'
import { ROOMS, roomName, figuresOf, roomOf, totalsOf, yakuName, type GameRecord, type Room } from '@/lib/history'
import { rungOf, type AccountLevels } from '@/lib/ladder'
import { signed, type MonitorConfig } from '@/lib/monitor'
import { tr } from '@/lib/i18n'

/** Rank points game by game, summed as they go: one line, and where zero is. */
function Spark({ points, label }: { points: number[]; label: string }) {
  const w = 120, h = 40
  if (points.length === 0) return <svg className="mstat-spark" viewBox={`0 0 ${w} ${h}`} aria-hidden />
  const all = [0, ...points]
  const lo = Math.min(...all), hi = Math.max(...all)
  const y = (v: number) => (hi === lo ? h / 2 : 2 + (h - 4) * (hi - v) / (hi - lo))
  const x = (i: number) => (all.length === 1 ? 0 : w * i / (all.length - 1))
  return <svg className="mstat-spark" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" role="img" aria-label={label}>
    <line x1={0} x2={w} y1={y(0)} y2={y(0)} />
    <polyline points={all.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ')} />
  </svg>
}

const share = (part: number, whole: number) => (whole > 0 ? `${(100 * part / whole).toFixed(1)}%` : '—')
const points = (n: number) => new Intl.NumberFormat('en-US').format(n)

/** Where we stand on the ladder of the table size we play, as the platform
 *  last said it (at login, and after every game): the rank, its points, and a
 *  bar to the next star. Three places of fixed width, so nothing moves as the
 *  points change; the bar's place is kept where there is no next star. */
function Ladder({ account, players }: { account: AccountLevels | null; players: number }) {
  const level = (players === 3 ? account?.level3 : account?.level) ?? null
  const rung = level && account ? rungOf(level, account.platform) : null
  const when = account ? new Date(account.at).toLocaleTimeString('zh-CN', { hour12: false, hour: '2-digit', minute: '2-digit' }) : ''
  const ceiling = rung?.ceiling ?? null
  return <span className="mstat-ladder" title={rung ? tr('客户端在 {0} 报告的段位，每场结束后更新', when) : tr('尚未收到客户端段位数据')}>
    <strong>{rung?.name ?? '—'}</strong>
    <span>{rung ? `${ceiling != null ? `${points(rung.score)} / ${points(ceiling)}` : points(rung.score)}${account?.platform === 'tenhou' ? ' pt' : ''}` : ''}</span>
    <i data-empty={ceiling == null || undefined} style={{ '--share': rung && ceiling ? Math.max(0, Math.min(1, rung.score / ceiling)) : 0 } as CSSProperties} />
  </span>
}

/** Our record over the games on file, in three panels that keep their places
 *  whatever is in them: where we stand now, rank points and placement; the
 *  figures a riichi player looks for first; and the yaku our wins were made
 *  of. The last two scroll. It opens on the room we play in now, the latest
 *  game's; another range picked holds while the tab is open.
 *
 *  Account reports and committed game records are independent observations.
 *  Records refresh after persistence; the rank changes only on a native report. */
export function Statistics({ historyRevision, account, platform }: { historyRevision: number; account: AccountLevels | null; platform: MonitorConfig['platform'] }) {
  const [records, setRecords] = useState<GameRecord[] | null>(null)
  const [failed, setFailed] = useState<string | null>(null)
  const [picked, setPicked] = useState<Room | 'all' | null>(null)
  useEffect(() => {
    let live = true
    void invoke<GameRecord[]>('get_history').then(
      found => { if (live) { setRecords(found); setFailed(null) } },
      e => { if (live) setFailed(String(e)) },
    )
    return () => { live = false }
  }, [historyRevision])

  const rooms = useMemo(() => ROOMS.filter(key => (records ?? []).some(r => roomOf(r) === key)), [records])
  // The core lists the games oldest first.
  const latest = records?.at(-1)
  const room = picked ?? (latest ? roomOf(latest) : 'all')
  const shown = useMemo(() => (records ?? []).filter(r => room === 'all' || roomOf(r) === room), [records, room])
  const t = useMemo(() => totalsOf(shown), [shown])
  const figures = useMemo(() => figuresOf(t), [t])
  const waiting = failed ? tr('读不到对局记录：{0}', failed) : records == null ? tr('正在读取对局记录') : null

  return <div className="md-page mstats">
    <section className="mstat mstat-rank" aria-label={platform === 'Tenhou' ? tr('段位pt与顺位') : tr('段位分与顺位')}>
      <header className="msec-head">
        <Ladder account={account} players={latest?.num_players ?? 4} />
        <select className="mselect" aria-label={tr('统计范围')} value={room} onChange={e => setPicked(e.target.value as Room | 'all')}>
          <option value="all">{tr('全部场次')}</option>
          {rooms.map(key => <option key={key} value={key}>{roomName(key)}</option>)}
        </select>
      </header>
      <div className="mstat-points">
        <p>
          <strong title={platform === 'Tenhou' ? tr('所选记录的累计规则pt变化') : undefined}>{t.pointGames > 0 ? signed(t.points.at(-1)!) : '—'}</strong>
          <small title={platform === 'Tenhou' ? tr('按天凤规则计算') : undefined}>{waiting ?? (platform === 'Tenhou'
            ? t.pointGames > 0 ? tr('规则pt · {0} 场', t.pointGames) : tr('还没有记录到段位pt')
            : t.pointGames > 0 ? tr('{0} 场的段位分变化', t.pointGames) : tr('还没有记录到段位分'))}</small>
        </p>
        <p className="mstat-sum"><span>{tr('平均顺位')} <b>{t.averagePlace != null ? t.averagePlace.toFixed(2) : '—'}</b></span><span>{tr('{0} 场', t.games)}</span></p>
        <Spark points={t.points} label={platform === 'Tenhou' ? tr('段位pt走势') : tr('段位分走势')} />
      </div>
      <ol className="mstat-places">
        {t.places.map((n, i) => <li key={i}>
          <span>{tr('{0} 位', i + 1)}</span>
          <i style={{ '--share': t.placementGames > 0 ? n / t.placementGames : 0 } as CSSProperties} />
          <b>{n}</b>
          <small>{share(n, t.placementGames)}</small>
        </li>)}
      </ol>
    </section>

    <section className="mstat mstat-figures">
      <header className="msec-head"><h3>{tr('数据')}</h3><span title={t.detailGames < t.games ? tr('详细统计仅使用 {0}/{1} 场完整记录', t.detailGames, t.games) : undefined}>{t.detailGames < t.games ? tr('{0} 局 · {1}/{2} 场', t.hands, t.detailGames, t.games) : tr('{0} 局', t.hands)}</span></header>
      <dl className="mstat-scroll">
        {figures.map(f => <div key={f.name} className="mfact" title={f.note}><dt>{f.name}</dt><dd>{f.value}</dd></div>)}
      </dl>
    </section>

    <section className="mstat mstat-yakus">
      <header className="msec-head"><h3>{tr('役种')}</h3><span>{t.scoredWins === t.wins ? tr('{0} 次和了', t.wins) : tr('{0}/{1} 次和了', t.scoredWins, t.wins)}</span></header>
      {t.yaku.length === 0 ? <p className="mslot-empty">{t.wins > 0 ? tr('已和了，但役种数据未记录') : tr('暂无和了记录')}</p>
        : <ol className="mstat-scroll">
          {t.yaku.map(y => <li key={`${y.platform}:${y.id}`} className="mfact mstat-yaku"><span>{yakuName(y.id, y.platform)}</span><b>{y.wins}</b><small>{share(y.wins, t.scoredWins)}</small></li>)}
        </ol>}
    </section>
  </div>
}
