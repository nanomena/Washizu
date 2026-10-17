import { useCallback, useEffect, useRef, useState, type CSSProperties } from 'react'
import { LoaderCircle, Pin, PinOff, X } from 'lucide-react'
import { CandidateList, DecisionCard } from '@/components/monitor/Decision'
import { GameStage } from '@/components/monitor/GameStage'
import { HandStrip, ResultCard, ScoreStrip, SeatColumns } from '@/components/monitor/Table'
import { AutoplayLevels } from '@/components/monitor/AutoplayLevel'
import { FaultCard } from '@/components/monitor/Details'
import { GamePages } from '@/components/monitor/Game'
import { Settings } from '@/components/monitor/Settings'
import { Statistics } from '@/components/monitor/Stats'
import { Faces } from '@/components/monitor/Tiles'
import { useMonitor } from '@/hooks/useMonitor'
import { pickShow } from '@/lib/botShow'
import { rememberGroup, savedGroup, type Group } from '@/lib/settingsGroup'
import {
  MONITOR_DEFAULTS, alertsOf, candidatesOf, decisionView, healthOf, levelOf, linkOf, roundLabel, seatWind, tableOf,
  type Alert, type AutoplayLevel, type Candidate, type DecisionView, type GameView, type Health, type MonitorConfig, type MonitorSnapshot,
} from '@/lib/monitor'
import { invoke, listen } from '@/lib/host'
import './Monitor.css'
import { localized, tr, useLang } from '@/lib/i18n'

// The application is this one window, and the window is a fixed canvas: drawn
// at the size below and scaled as one piece to whatever size the window is.
// The shell keeps the window at the canvas's aspect ratio (`desktop/`), so in
// practice the canvas always fills it; if something else resizes the window,
// the canvas is centred and nothing is cut off.
//
//   1280 × 960 (4:3) — the game on a 16:9 stage, and a 1280 × 240 dock under it
const STAGE = { w: 1280, h: 720 }
const DOCK = { w: 1280, h: 240 }

// From the move to the game: this decision, the hand it is in, the game the hand
// is in. Then the record over games, and the settings, which hold the links.
type Tab = 'decision' | 'hand' | 'game' | 'stats' | 'settings'
const TABS: Array<[Tab, string]> = localized([['decision', '决策'], ['hand', '本局'], ['game', '对局'], ['stats', '统计'], ['settings', '设置']])

/** The stage the game is shown in may not be smaller than this; `resize_monitor_game` rejects less than 100. */
const STAGE_MIN = 160

function useNow(running: boolean, every: number): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!running) return
    const timer = setInterval(() => setNow(Date.now()), every)
    return () => clearInterval(timer)
  }, [running, every])
  return now
}

/** How much the `w × h` canvas has to be scaled to fit the window. */
function useFit(w: number, h: number): number {
  const fit = useCallback(() => Math.min(window.innerWidth / w, window.innerHeight / h) || 1, [w, h])
  const [k, setK] = useState(fit)
  useEffect(() => {
    const apply = () => setK(fit())
    apply()
    window.addEventListener('resize', apply)
    return () => window.removeEventListener('resize', apply)
  }, [fit])
  return k
}

/** Everything the dock draws from, worked out once. */
type Model = {
  s: MonitorSnapshot | null
  view: DecisionView
  rows: Candidate[]
  listState: 'live' | 'done' | 'void'
  noRows: string
  /** Faults that leave no valid suggestion; they take the candidates' place. */
  faults: Alert[]
  health: Health[]
  elapsed: number
  now: number
}

function Round({ s }: { s: MonitorSnapshot | null }) {
  const game = s?.game
  return <div className="mround">
    {game ? <>
      <strong>{roundLabel(game)}</strong>
      {/* Who the dealer is, the scores say. */}
      <span className="mround-seat">{game.our_seat != null && <em>{tr('{0}家', seatWind(game.our_seat, game))}</em>}</span>
      {/* The tiles turned over are indicators — the dora is the tile after each. Room for five, the most a hand can turn over. */}
      <span className="mdora" title={tr('宝牌指示牌')}>{game.dora_markers.length > 0 && <Faces faces={game.dora_markers.map(tile => ({ tile }))} />}</span>
      <span className="mround-count">{game.honba} {tr('本场 · 供托')} {game.kyotaku}</span>
    </> : <span className="mround-idle"><strong>{s?.brand ?? ''}</strong><span>{s?.game_ended === false ? tr('等待牌局数据') : tr('等待对局')}</span></span>}
  </div>
}

/** The candidates' slot: candidates, or the reason there are none. */
function Slot({ m, onDetails }: { m: Model; onDetails: () => void }) {
  if (m.faults.length > 0 && m.rows.length === 0) return <FaultCard alerts={m.faults} onDetails={onDetails} />
  // What the numbers are is the bot's to say; its card carries the words.
  const card = pickShow(m.s?.decision?.response?.meta)
  return <CandidateList rows={m.rows} state={m.listState} empty={m.noRows} loading={m.view.phase === 'thinking'} caption={card?.caption} hint={card?.hint} />
}

/** 1280 × 240, under the game. The decision never leaves its place on the
 *  left; the tabs change what stands to the right of it. */
type DockProps = {
  m: Model; tab: Tab; setTab: (tab: Tab) => void; tools: React.ReactNode
  options: MonitorConfig
  onLevel: ((level: AutoplayLevel) => void) | null
  onPin: ((pinned: boolean) => void) | null
  onTrail: ((enabled: boolean) => void) | null
  onLean: ((enabled: boolean) => void) | null
  onConfirm: ((enabled: boolean) => void) | null
  onAutoWin: ((enabled: boolean) => void) | null
  onStream: ((enabled: boolean) => void) | null
  onCopyStream: (() => Promise<void>) | null
  onGameView: ((view: GameView) => void) | null
  onSpout: ((enabled: boolean) => void) | null
}
function Dock({ m, tab: chosen, setTab, tools, options, onLevel, onPin, onTrail, onLean, onConfirm, onAutoWin, onStream, onCopyStream, onGameView, onSpout }: DockProps) {
  const { s, view } = m
  const game = s?.game ?? null
  const link = linkOf(m.health)
  // A tab shows what it holds while the pointer rests on it; a click keeps it.
  // Leaving the dock goes back to the tab kept. The pause keeps a pointer on
  // its way across the tabs from flicking through them.
  const [peek, setPeek] = useState<Tab | null>(null)
  const pending = useRef<number | undefined>(undefined)
  const hover = (key: Tab | null) => {
    window.clearTimeout(pending.current)
    if (key === null) setPeek(null)
    else pending.current = window.setTimeout(() => setPeek(key), 140)
  }
  useEffect(() => () => window.clearTimeout(pending.current), [])
  const tab = peek ?? chosen
  const [group, setGroup] = useState<Group>(savedGroup)
  const openGroup = (next: Group) => { setGroup(next); rememberGroup(next) }
  return <aside className="md" onMouseLeave={() => hover(null)}>
    <header className="md-bar">
      <Round s={s} />
      <div className="md-controls">
        {/* The control is the setting, and says so by its colour: how far the model's actions may be carried out — not that one has been. */}
        <span className="md-level"><span aria-hidden>{tr('自动操作')}</span><AutoplayLevels level={levelOf(s, options)} onLevel={onLevel} /></span>
        <div className="mtools">{tools}</div>
      </div>
      <nav className="md-tabs" role="tablist" aria-label={tr('监视内容')}>
        {/* The links have one light, on the settings' tab, where they are: everything behind it is run by this window. */}
        {TABS.map(([key, label]) => <button key={key} role="tab" aria-selected={tab === key} data-kept={chosen === key || undefined}
          onClick={() => { hover(null); setTab(key) }} onMouseEnter={() => hover(key)} onMouseLeave={() => window.clearTimeout(pending.current)}
          title={key === 'settings' ? tr('连接：{0}', link.summary) : undefined}>
          {label}{key === 'settings' && <i className="mdot md-tab-dot" data-tone={link.tone} />}
        </button>)}
      </nav>
    </header>

    <div className="md-body" role="tabpanel" data-tab={tab}>
      <DecisionCard view={view} decision={s?.decision ?? null} elapsed={m.elapsed} past={!!s?.round_ended} />
      {/* At the end of a round its result stands where the candidates and the hand were. */}
      {tab === 'decision' && (s?.result ? <ResultCard result={s.result} snapshot={s} />
        : <>
          <Slot m={m} onDetails={() => { openGroup('link'); setTab('settings') }} />
          {game ? <div className="md-side"><HandStrip game={game} suggestion={view.live ? s?.decision?.response ?? null : null} /><ScoreStrip game={game} /></div>
            : <p className="mslot-empty">{tr('对局开始后，这里显示手牌和四家点数')}</p>}
        </>)}
      {tab === 'hand' && (s?.game ? <SeatColumns snapshot={s} /> : <p className="mslot-empty md-wide">{tr('对局开始后，这里显示公开的牌河、副露和点数')}</p>)}
      {tab === 'game' && (s ? <GamePages key={s.context.game} snapshot={s} /> : <p className="mslot-empty md-wide">{tr('正在连接监视数据')}</p>)}
      {/* The record can change when a game starts or ends, and at no other time. */}
      {tab === 'stats' && <Statistics refresh={`${s?.context.game ?? 0}:${s?.game_ended ?? true}`} />}
      {tab === 'settings' && <Settings group={group} onGroup={openGroup} snapshot={s} options={options} health={m.health} now={m.now} onPin={onPin} onLevel={onLevel} onTrail={onTrail} onLean={onLean}
        onConfirm={onConfirm} onAutoWin={onAutoWin}
        onStream={onStream} onCopyStream={onCopyStream} onGameView={onGameView} onSpout={onSpout} />}
    </div>
  </aside>
}

export function Monitor() {
  const { snapshot: s, error: bridgeError } = useMonitor()
  // Everything below is drawn in the language of the moment; the dock, which
  // remembers words it has worked out, is made anew when the language changes.
  const lang = useLang()
  const [options, setOptions] = useState(MONITOR_DEFAULTS)
  const [ready, setReady] = useState(false)
  const [tab, setTab] = useState<Tab>('decision')
  const [busy, setBusy] = useState(false)
  const [opened, setOpened] = useState(false)
  const [uiError, setUiError] = useState<string | null>(null)
  const stage = useRef<HTMLDivElement>(null)

  useEffect(() => {
    let cancelled = false
    invoke<MonitorConfig>('get_monitor_options').then(
      saved => { if (!cancelled) { setOptions(saved); setReady(true) } },
      e => { if (!cancelled) setUiError(String(e)) },
    )
    return () => { cancelled = true }
  }, [])
  // Options the shell changes by itself: the Spout sender starting or stopping, the game view given up.
  useEffect(() => listen<MonitorConfig>('monitor-options', setOptions), [])

  const k = useFit(STAGE.w, STAGE.h + DOCK.h)
  // Drawn offscreen, the game is a picture on the stage rather than a view over it.
  const offscreen = options.game_view_now === 'offscreen'
  // The game view outlives a reload of this page, and capture says so: it only
  // ever runs from a view the shell has made.
  const embedded = opened || (s != null && s.capture.state !== 'stopped')

  // The game is a native child view, not DOM: it goes exactly where the stage
  // element is, and has to be told every time that moves or goes away.
  const place = useCallback(() => {
    const r = stage.current?.getBoundingClientRect()
    const bounds = r && r.width >= STAGE_MIN && r.height >= STAGE_MIN
      ? { x: Math.max(0, r.x), y: Math.max(0, r.y), width: r.width, height: r.height, visible: true }
      : { x: 0, y: 0, width: STAGE_MIN, height: STAGE_MIN, visible: false }
    void invoke('resize_monitor_game', { bounds }).catch(e => setUiError(String(e)))
  }, [])
  useEffect(() => {
    if (!embedded) return
    place()
    const el = stage.current
    if (!el) return
    const observer = new ResizeObserver(place)
    observer.observe(el)
    return () => observer.disconnect()
  }, [embedded, place])

  const pin = async (pinned: boolean) => {
    setBusy(true); setUiError(null)
    try { setOptions(await invoke<MonitorConfig>('set_monitor_pinned', { pinned })) }
    catch (e) { setUiError(String(e)) }
    finally { setBusy(false) }
  }
  /** A window option the shell saves and answers with all of them. */
  const choose = async (command: string, enabled: boolean) => {
    setBusy(true); setUiError(null)
    try { setOptions(await invoke<MonitorConfig>(command, { enabled })) }
    catch (e) { setUiError(String(e)) }
    finally { setBusy(false) }
  }
  const chooseGameView = async (view: GameView) => {
    setBusy(true); setUiError(null)
    try { setOptions(await invoke<MonitorConfig>('set_monitor_game_view', { view })) }
    catch (e) { setUiError(String(e)) }
    finally { setBusy(false) }
  }
  const copyStream = async () => {
    try { await invoke('copy_stream_url') }
    catch (e) { setUiError(String(e)); throw e }
  }
  const openGame = useCallback(async () => {
    const r = stage.current?.getBoundingClientRect()
    if (!r) return
    setBusy(true); setUiError(null)
    try {
      await invoke('open_monitor_game', { bounds: { x: Math.max(0, r.x), y: Math.max(0, r.y), width: Math.max(100, r.width), height: Math.max(100, r.height), visible: true } })
      setOpened(true)
    } catch (e) { setUiError(String(e)) }
    finally { setBusy(false) }
  }, [])
  // The window is the game: it is opened without being asked, once, as soon as
  // there is a stage to put it on. If that fails the stage keeps its button,
  // and the reason is shown.
  const asked = useRef(false)
  useEffect(() => {
    if (asked.current || !ready || !s || embedded) return
    asked.current = true
    void openGame()
  }, [ready, s, embedded, openGame])
  // Playing on is saved first, so that autoplay switched on is already at its level.
  const setLevel = async (level: AutoplayLevel) => {
    setUiError(null)
    try {
      if (level !== 'off') setOptions(await invoke<MonitorConfig>('set_monitor_continue', { enabled: level === 'continue' }))
      await invoke('set_monitor_autoplay', { enabled: level !== 'off' })
    } catch (e) { setUiError(String(e)) }
  }

  const table = tableOf(s?.game)
  const d = s?.decision ?? null
  const view = decisionView(s)
  const thinking = view.phase === 'thinking'
  const now = useNow(thinking || tab === 'settings', thinking ? 100 : 1000)
  const alerts = alertsOf(s, { ui: uiError, bridge: bridgeError })
  const local = alerts.filter(a => a.key === 'ui' || a.key === 'bridge')
  const faults = alerts.filter(a => a.key !== 'ui' && a.key !== 'bridge')

  // Candidates stand only while their decision does. Once the game has
  // answered with a different move they stay as a record of what was weighed,
  // marked void; a fault or the end of the round drops them altogether.
  const sound = !!s && !s.round_ended && !s.sync_error && s.service !== 'error'
  const listState = view.phase === 'mismatch' ? 'void' : view.phase === 'confirmed' ? 'done' : 'live'
  const rows = sound && (view.live || view.phase === 'confirmed' || view.phase === 'mismatch') ? candidatesOf(d, table, view.instead) : []
  const noRows = !s ? tr('正在连接监视数据')
    : view.phase === 'idle' ? tr('轮到自己时，这里列出模型的候选动作')
    : view.phase === 'failed' || view.phase === 'unavailable' ? tr('这一步没有候选')
    : sound && (view.live || view.phase === 'confirmed') ? tr('模型这一步没有给出候选')
    : s.round_ended ? tr('本局已结束，候选不再有效')
    : tr('建议已失效，候选不再显示')
  const m: Model = { s, view, rows, listState, noRows, faults, health: healthOf(s), elapsed: d ? Math.max(0, now - d.started_ms) : 0, now }

  const tools = <button className="mtool" aria-label={tr('置顶')} title={options.always_on_top ? tr('取消置顶') : tr('窗口置顶')} aria-pressed={options.always_on_top} disabled={!ready || busy}
    onClick={() => void pin(!options.always_on_top)}>{options.always_on_top ? <Pin size={15} /> : <PinOff size={15} />}</button>

  return <main className="mon" data-lang={lang} style={{ '--k': k } as CSSProperties}>
    <div className="mon-fit">
      <div className="mon-stage" ref={stage}>
        <div className="mon-canvas mon-canvas--stage">
          {embedded && offscreen ? <GameStage />
          : embedded ? <div className="mon-stage-idle">
            <LoaderCircle size={22} className="mspin" />
            <p>{tr('雀魂视图在这里显示')}</p>
            <button className="mbtn" onClick={place}>{tr('重新对齐游戏视图')}</button>
          </div>
            : <div className="mon-stage-start">
              <h2>{s?.brand ? tr('{0} · 雀魂', s.brand) : tr('雀魂')}</h2>
              <p>{tr('上面登录雀魂并对局，下面核对模型建议和雀魂的实际动作。')}</p>
              <div className="mon-start-card">
                <p>{tr('游戏使用独立的登录资料，只在这个窗口里用。')}</p>
                <button className="mbtn mbtn--primary" disabled={!ready || busy} onClick={() => void openGame()}>
                  {busy ? <><LoaderCircle size={16} className="mspin" />{tr('正在连接游戏视图…')}</> : tr('打开雀魂')}
                </button>
              </div>
            </div>}
        </div>
      </div>

      <div className="mon-canvas mon-canvas--dock">
        <Dock key={lang} m={m} tab={tab} setTab={setTab} tools={tools} options={options}
          onLevel={ready ? level => void setLevel(level) : null} onPin={ready && !busy ? pinned => void pin(pinned) : null}
          onTrail={ready && !busy ? enabled => void choose('set_monitor_trail', enabled) : null}
          onLean={ready && !busy ? enabled => void choose('set_monitor_lean', enabled) : null}
          onConfirm={ready && !busy ? enabled => void choose('set_monitor_confirm', enabled) : null}
          onAutoWin={ready && !busy ? enabled => void choose('set_monitor_auto_win', enabled) : null}
          onStream={ready && !busy ? enabled => void choose('set_monitor_stream', enabled) : null}
          onCopyStream={ready ? copyStream : null}
          onGameView={ready && !busy ? view => void chooseGameView(view) : null}
          onSpout={ready && !busy ? enabled => void choose('set_monitor_spout', enabled) : null} />
        {local.length > 0 && <div className="mtoast" role="alert">
          <div><strong>{local[0].title}</strong><p>{local[0].message}</p></div>
          {uiError && <button aria-label={tr('知道了')} onClick={() => setUiError(null)}><X size={13} /></button>}
        </div>}
      </div>
    </div>
  </main>
}
