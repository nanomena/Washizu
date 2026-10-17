import { relativeKind, type RelativeKind } from '@/lib/format'
import { pickShow, visibleItems } from '@/lib/botShow'
import type { BotResponse, BotStatus, CaptureStatus, GameStateSnapshot, MahgenView, MeldSnapshot, MjaiEvent, ShowItem } from '@/types'
import { localized, tr } from '@/lib/i18n'

/** The window's own options, and which build it is (`version` comes from the shell).
 *  `stream_url` is the address the shell serves the game at, while it does. */
export type MonitorConfig = {
  always_on_top: boolean; pointer_trail?: boolean; pointer_lean?: boolean
  quick_confirm?: boolean; riichi_auto_win?: boolean
  /** Play game after game: with autoplay on, the game's own screens after its end are pressed through to "play again". */
  auto_continue?: boolean
  stream_output?: boolean; stream_port?: number; stream_token?: string; stream_url?: string
  version?: string
  /** How the game is shown from the next start, and how it is shown now: in the window, or drawn offscreen. */
  game_view?: GameView; game_view_now?: GameView
  /** Whether the game's frames go to a streaming program over Spout (Windows, offscreen only), and how that is going. */
  spout_output?: boolean; spout_available?: boolean; spout_state?: 'off' | 'starting' | 'sending' | 'failed' | 'stopped'; spout_name?: string
}
export type GameView = 'window' | 'offscreen'
export const MONITOR_DEFAULTS: MonitorConfig = { always_on_top: false, pointer_trail: false, pointer_lean: false, quick_confirm: false, riichi_auto_win: false, auto_continue: false }

/** How far autoplay goes: not at all (suggestions only), the game in play, or
 *  game after game. Two settings make it: autoplay itself, which the core
 *  reports in the snapshot, and playing on, which is only read while autoplay is on. */
export type AutoplayLevel = 'off' | 'play' | 'continue'
export const AUTOPLAY_LEVELS: readonly AutoplayLevel[] = ['off', 'play', 'continue']
export function levelOf(s: MonitorSnapshot | null, options: MonitorConfig): AutoplayLevel | null {
  if (!s) return null
  return !s.autoplay_enabled ? 'off' : options.auto_continue ? 'continue' : 'play'
}
/** What a level does, in a line. */
export function levelAbout(level: AutoplayLevel): string {
  return level === 'off' ? tr('只显示建议，不替你操作')
    : level === 'play' ? tr('替你打牌；一场结束后，结束画面留给你')
    : tr('替你打牌；一场结束后点过结束画面，再来一场')
}
export type DecisionContext = { game: number; round: number; event_seq: number }
export type MonitorDecision = {
  /** Which decision this is, for the life of the process; `number` is its place among its game's. */
  id: number; number: number; context: DecisionContext; started_ms: number; completed_ms: number | null
  reaction_ms: number | null; bot: string; trigger: MjaiEvent
  state: 'thinking' | 'awaiting' | 'confirmed' | 'expired' | 'failed' | 'unavailable' | 'declined'
  response: BotResponse | null; error: string | null; confirmed_seq: number | null
}
/** A hand the round's end made public. `melds` are its open melds as the round
 *  ended, kept with it because the table is dealt again at the next round. */
export type PublicHand = { seat: number; tiles: string[]; melds?: MeldSnapshot[]; winning_tile: string | null; target: number | null; ura_markers: string[] }
export type RoundResult = { kind: string; hands: PublicHand[]; scores: number[] | null; deltas: number[] | null }
export type MonitorEvent = { seq: number; ts_ms: number; event: MjaiEvent }
/** One hand of the current game: which it was and how it ended (`null` while it
 *  is played). Its events are asked for by `round` (`get_hand_events`). */
export type HandSummary = { round: number; bakaze: string; kyoku: number; honba: number; result: RoundResult | null; events: number }
/** One thing a bot or its service says about itself. Shown as written: this
 *  window does not know what any of it means. */
export type Fact = { name: string; value: string; note?: string }
/** The bot's service as a process the app runs: whether it is up. `service` on
 *  the snapshot is a different fact — whether the bot has reached it. */
export type ServerStatus =
  | { state: 'none' }
  | { state: 'installing'; bot: string }
  | { state: 'starting'; bot: string; since_ms: number }
  | { state: 'ready'; bot: string; label: string | null; facts: Fact[]; since_ms: number; startup_ms: number }
  | { state: 'failed'; bot: string; error: string; retry_at_ms: number }
export type MonitorSnapshot = {
  revision: number; context: DecisionContext; names: string[]; red_fives: boolean | null
  game: GameStateSnapshot | null; view: MahgenView | null; round_ended: boolean; game_ended: boolean
  capture: CaptureStatus; bot: BotStatus; service: string; service_error: string | null; server: ServerStatus
  sync_error: string | null
  brand: string; facts: Fact[]; autoplay_enabled: boolean
  decision: MonitorDecision | null; previous_decision: MonitorDecision | null
  result: RoundResult | null; previous_result: RoundResult | null
  /** The hands of this game in the order dealt, the one in play last. */
  hands: HandSummary[]
  events: MonitorEvent[]; last_event_ms: number | null
}

export function newestSnapshot(current: MonitorSnapshot | null, incoming: MonitorSnapshot): MonitorSnapshot {
  return current && current.revision > incoming.revision ? current : incoming
}

// ---------------------------------------------------------------- tiles ----

/** One drawn tile. `side` lays it down (called tile, riichi declaration);
 *  `back` hides the face (concealed kan, hidden hands); `called` is a discard
 *  another seat took, drawn as an outline where it lay. */
export type Face = { tile: string; side?: boolean; back?: boolean; dim?: boolean; called?: boolean }

const HONOR: Record<string, string> = localized({ E: '东', S: '南', W: '西', N: '北', P: '白', F: '发', C: '中' })
const SUIT: Record<string, string> = localized({ m: '万', p: '筒', s: '索' })
export const WIND: Record<string, string> = localized({ E: '东', S: '南', W: '西', N: '北' })

export function tileLabel(tile: string): string {
  if (!tile || tile === '?') return tr('未公开')
  if (HONOR[tile]) return HONOR[tile]
  return `${tile.endsWith('r') ? tr('赤') : ''}${tile[0]}${SUIT[tile[1]] ?? ''}`
}
/** A tile lying on its side, in upright-tile widths. */
export const SIDE = 92 / 70
/** A pon or chi, and an open kan (the widest meld there is), in tile-widths. */
const PON = 2 + SIDE
const KAN = 3 + SIDE
/** The room a hand is given, in tile-widths: thirteen tiles and the draw
 *  standing a little apart, with four sets called away — three pons and a kan.
 *  Every hand that comes up in play fits it, so its tiles are one size. */
export const HAND_ROOM = 13 + 1.3 - 4 * 3 + 3 * (PON + 0.3) + (KAN + 0.3)
/** The room `n` melds in a row are given: pons, one of them a kan. */
export const meldsRoom = (n: number) => n * (PON + 0.3) - 0.3 + (KAN - PON)

/** Red fives are a distinct identity everywhere except "is this the same kind of tile". */
const plain = (tile: string) => tile.replace(/r$/, '')
const rank = (tile: string) => (HONOR[tile] ? 40 + 'ESWNPFC'.indexOf(tile) : 'mps'.indexOf(tile[1]) * 10 + Number(tile[0])) + (tile.endsWith('r') ? 0.5 : 0)
const sorted = (tiles: string[]) => [...tiles].sort((a, b) => rank(a) - rank(b))

// ---------------------------------------------------------------- seats ----

export const REL_LABEL: Record<RelativeKind, string> = localized({ self: '自己', shimocha: '下家', toimen: '对家', kamicha: '上家' })
export const REL_SHORT: Record<RelativeKind, string> = localized({ self: '自', shimocha: '下', toimen: '对', kamicha: '上' })
export type Table = { self: number | null; players: number }
export const tableOf = (game: GameStateSnapshot | null | undefined): Table => ({ self: game?.our_seat ?? null, players: game?.num_players ?? 4 })
export const relOf = (seat: number, t: Table): RelativeKind | null => (t.self == null ? null : relativeKind(seat, t.self, t.players))
export function seatLabel(seat: number, t: Table): string {
  const rel = relOf(seat, t)
  return rel ? REL_LABEL[rel] : tr('座位 {0}', seat + 1)
}
export function seatWind(seat: number, game: GameStateSnapshot): string {
  return WIND['ESWN'[(seat - game.oya + game.num_players) % game.num_players]]
}
/** A hand by its place in the game: 东1局 2 本场. */
export function handLabel(h: { bakaze: string; kyoku: number; honba: number }): string {
  return tr('{0}{1}局 {2} 本场', WIND[h.bakaze] ?? h.bakaze, h.kyoku, h.honba)
}
export function roundLabel(game: GameStateSnapshot): string {
  return tr('{0}{1}局', WIND[game.bakaze] ?? game.bakaze, game.kyoku)
}

// -------------------------------------------------------------- actions ----

export type ActionTone = 'discard' | 'riichi' | 'call' | 'kan' | 'win' | 'pass' | 'draw'
export type ActionView = {
  tone: ActionTone
  /** 打 / 立直 / 吃 / 碰 / 明杠 / 暗杠 / 加杠 / 自摸 / 荣和 / 过 */
  verb: string
  /** What the verb applies to, in words: "9索". Empty when the verb says it all. */
  object: string
  faces: Face[]
  /** 摸切 / 手切 — only known for a discard, or a riichi's, that the model or the game named. */
  note?: string
}

/** A called tile lies sideways on the side it came from: 上家 left, 对家 middle, 下家 right. */
function meldFaces(called: string, own: string[], from: number | null, actor: number, t: Table): Face[] {
  const rel = from == null ? null : relativeKind(from, actor, t.players)
  const faces: Face[] = sorted(own).map(tile => ({ tile }))
  const at = rel === 'shimocha' ? faces.length : rel === 'toimen' ? 1 : 0
  faces.splice(at, 0, { tile: called, side: true })
  return faces
}
function concealedKan(tiles: string[]): Face[] {
  // Keep a red five among the two faces that stay visible.
  const four = [...tiles].sort((a, b) => Number(b.endsWith('r')) - Number(a.endsWith('r')))
  return [{ tile: four[2] ?? '?', back: true }, { tile: four[0] ?? '?' }, { tile: four[1] ?? '?' }, { tile: four[3] ?? '?', back: true }]
}

export function describeAction(action: MjaiEvent | null | undefined, t: Table): ActionView | null {
  if (!action) return null
  switch (action.type) {
    case 'dahai': return { tone: 'discard', verb: tr('打'), object: tileLabel(action.pai), faces: [{ tile: action.pai }], note: action.tsumogiri ? tr('摸切') : tr('手切') }
    case 'reach': return {
      tone: 'riichi', verb: tr('立直'), object: action.pai ? tr('打 {0}', tileLabel(action.pai)) : '', faces: action.pai ? [{ tile: action.pai }] : [],
      note: action.tsumogiri === undefined ? undefined : action.tsumogiri ? tr('摸切') : tr('手切'),
    }
    case 'chi': return { tone: 'call', verb: tr('吃'), object: tileLabel(action.pai), faces: meldFaces(action.pai, action.consumed, action.target, action.actor, t) }
    case 'pon': return { tone: 'call', verb: tr('碰'), object: tileLabel(action.pai), faces: meldFaces(action.pai, action.consumed, action.target, action.actor, t) }
    case 'daiminkan': return { tone: 'kan', verb: tr('明杠'), object: tileLabel(action.pai), faces: meldFaces(action.pai, action.consumed, action.target, action.actor, t) }
    case 'ankan': return { tone: 'kan', verb: tr('暗杠'), object: tileLabel(plain(action.consumed[0] ?? '?')), faces: concealedKan(action.consumed) }
    case 'kakan': return { tone: 'kan', verb: tr('加杠'), object: tileLabel(action.pai), faces: [{ tile: action.pai }] }
    case 'kita': return { tone: 'call', verb: tr('拔北'), object: '', faces: [{ tile: action.pai ?? 'N' }] }
    case 'hora': return { tone: 'win', verb: action.actor === action.target ? tr('自摸') : tr('荣和'), object: action.pai ? tileLabel(action.pai) : '', faces: action.pai ? [{ tile: action.pai }] : [] }
    case 'ryukyoku': return { tone: 'draw', verb: tr('九种九牌'), object: tr('宣告流局'), faces: [] }
    case 'none': return { tone: 'pass', verb: tr('过'), object: '', faces: [] }
    default: return null
  }
}
export function actionLabel(action: MjaiEvent | null | undefined, t: Table = { self: null, players: 4 }): string {
  const view = describeAction(action, t)
  if (!view) return action ? action.type : tr('等待建议')
  return view.object ? `${view.verb} ${view.object}` : view.verb
}

/** The moves that commit a seat. A riichi declaration alone commits nothing. */
const COMMITS = new Set(['dahai', 'chi', 'pon', 'daiminkan', 'ankan', 'kakan', 'kita', 'hora'])
const actorOf = (e: MjaiEvent): number | null => ('actor' in e && typeof e.actor === 'number' ? e.actor : null)

// ------------------------------------------------------------ candidates ----

export type Candidate = {
  tone: ActionTone; verb: string; faces: Face[]
  /** 摸切 on a discard its bot says is the tile just drawn; empty otherwise. */
  note: string
  /** The row's value as a share, 0..1, when it is a percentage; null otherwise. */
  share: number | null
  value: string
  /** This row is the action the model actually answered with. */
  chosen: boolean
  /** This row is what Mahjong Soul did instead — only set once the two are known to differ. */
  taken: boolean
}

const sameTiles = (a: string[], b: string[]) => a.length === b.length && sorted(a).join() === sorted(b).join()

function candidateOf(item: ShowItem, d: MonitorDecision, t: Table): Omit<Candidate, 'share' | 'value' | 'chosen' | 'taken' | 'note'> & { note?: string; matches: (r: MjaiEvent) => boolean } {
  const pais = item.pais ?? []
  const from = actorOf(d.trigger)
  const self = t.self ?? actorOf(d.response ?? { type: 'none' }) ?? 0
  // Calls list the tiles taken from the hand first and the claimed tile last.
  const called = pais[pais.length - 1] ?? '?'
  const own = pais.slice(0, -1)
  const claims = (kind: string) => (r: MjaiEvent) => r.type === kind && 'consumed' in r && 'pai' in r && r.pai === called && sameTiles(r.consumed, own)
  switch (item.label) {
    case 'Discard': {
      // Said by the bot or not at all: without it, either copy of the tile is this row.
      const giri = typeof item.tsumogiri === 'boolean' ? item.tsumogiri : null
      return {
        tone: 'discard', verb: tr('打'), faces: pais.map(tile => ({ tile })), note: giri ? tr('摸切') : '',
        matches: r => r.type === 'dahai' && r.pai === pais[0] && (giri === null || r.tsumogiri === giri),
      }
    }
    case 'Riichi': {
      // The tile the declaration throws, and its copy, when the bot names them.
      const giri = typeof item.tsumogiri === 'boolean' ? item.tsumogiri : null
      return { tone: 'riichi', verb: tr('立直'), faces: pais.map(tile => ({ tile })), note: giri ? tr('摸切') : '', matches: r => r.type === 'reach' }
    }
    case 'Chi': return { tone: 'call', verb: tr('吃'), faces: meldFaces(called, own, from, self, t), matches: claims('chi') }
    case 'Pon': return { tone: 'call', verb: tr('碰'), faces: meldFaces(called, own, from, self, t), matches: claims('pon') }
    case 'Kan':
      // Only an open kan answers someone else's discard; the rest are the seat's own turn.
      if (d.trigger.type === 'dahai') return { tone: 'kan', verb: tr('明杠'), faces: meldFaces(called, own, from, self, t), matches: claims('daiminkan') }
      if (pais.length === 4) return { tone: 'kan', verb: tr('暗杠'), faces: concealedKan(pais), matches: r => r.type === 'ankan' && sameTiles(r.consumed, pais) }
      return { tone: 'kan', verb: tr('加杠'), faces: pais.slice(0, 1).map(tile => ({ tile: plain(tile) })), matches: r => r.type === 'kakan' && plain(r.pai) === plain(pais[0] ?? '') }
    case 'Ankan': return { tone: 'kan', verb: tr('暗杠'), faces: concealedKan(pais), matches: r => r.type === 'ankan' && sameTiles(r.consumed, pais) }
    case 'Kakan': return { tone: 'kan', verb: tr('加杠'), faces: pais.slice(0, 1).map(tile => ({ tile: plain(tile) })), matches: r => r.type === 'kakan' && plain(r.pai) === plain(pais[0] ?? '') }
    case 'Ron': return { tone: 'win', verb: tr('荣和'), faces: pais.map(tile => ({ tile })), matches: r => r.type === 'hora' && r.actor !== r.target }
    case 'Tsumo': return { tone: 'win', verb: tr('自摸'), faces: pais.map(tile => ({ tile })), matches: r => r.type === 'hora' && r.actor === r.target }
    case 'Hora': return { tone: 'win', verb: tr('和牌'), faces: pais.map(tile => ({ tile })), matches: r => r.type === 'hora' }
    case 'Pass': return { tone: 'pass', verb: tr('过'), faces: [], matches: r => r.type === 'none' }
    case 'Ryukyoku': case 'Nine terminals': return { tone: 'draw', verb: tr('九种九牌'), faces: [], matches: r => r.type === 'ryukyoku' }
    case 'Kita': return { tone: 'call', verb: tr('拔北'), faces: [{ tile: 'N' }], matches: r => r.type === 'kita' }
    default: return { tone: 'pass', verb: item.label ?? '—', faces: pais.map(tile => ({ tile })), matches: () => false }
  }
}

/** The model's ranked candidates. Percentages are shown exactly as reported:
 *  cutting the list to `limit` rows never renormalises them. `instead` is the
 *  move the game made when it contradicted the suggestion. */
export function candidatesOf(d: MonitorDecision | null, t: Table, instead: MjaiEvent | null = null, limit = 5): Candidate[] {
  if (!d?.response) return []
  const response = d.response
  let chosen = false, taken = false
  return visibleItems(pickShow(response.meta), limit).map(item => {
    const { matches, ...row } = candidateOf(item, d, t)
    const percent = /^\s*(\d+(?:\.\d+)?)\s*%\s*$/.exec(item.value ?? '')
    // Two rows can show the same tile (hand discard and tsumogiri). A bot that says
    // which is which is matched by it; otherwise the higher one is the answer.
    const isChosen = !chosen && matches(response)
    const isTaken = !taken && !isChosen && !!instead && matches(instead)
    chosen ||= isChosen
    taken ||= isTaken
    return { ...row, note: row.note ?? '', share: percent ? Math.min(1, Number(percent[1]) / 100) : null, value: item.value ?? '', chosen: isChosen, taken: isTaken }
  })
}

// -------------------------------------------------------------- decision ----

export type Phase = 'idle' | 'thinking' | 'awaiting' | 'confirmed' | 'mismatch' | 'expired' | 'failed' | 'unavailable' | 'declined'
/** A state's colour: `done` is what the game carried out, `ok` a part that works, `wait` and `busy` what is still to come. */
export type Tone = 'ok' | 'done' | 'wait' | 'busy' | 'bad' | 'idle'
export type StepState = 'done' | 'active' | 'todo' | 'bad' | 'void'
export type DecisionView = {
  phase: Phase; tone: Tone
  /** Short state word for the badge. */
  status: string
  /** One sentence saying what that state means right now. */
  message: string
  /** The suggestion is still the valid, current one. */
  live: boolean
  /** What the model answered. Kept after it stops being live, as history. */
  action: ActionView | null
  /** What Mahjong Soul actually did in answer to this decision, once known. */
  actual: ActionView | null
  /** The game's own event behind `actual`, when it contradicted the suggestion. */
  instead: MjaiEvent | null
  /** What the model was reacting to: "摸 2索", "下家 打 3筒". */
  trigger: string
  /** Request sent → suggestion returned → confirmed by the game. */
  steps: [StepState, StepState, StepState]
}

function triggerLabel(e: MjaiEvent, t: Table): string {
  const who = actorOf(e)
  const seat = who == null || who === t.self ? '' : `${seatLabel(who, t)} `
  if (e.type === 'tsumo') return e.pai === '?' ? tr('摸牌') : tr('摸 {0}', tileLabel(e.pai))
  if (e.type === 'dahai') return tr('{0}打 {1}', seat, tileLabel(e.pai))
  if (e.type === 'start_kyoku') return tr('开局')
  if (e.type === 'reach') return tr('{0}立直', seat)
  if (e.type === 'none') return ''
  return `${seat}${actionLabel(e, t)}`
}

/** Why a suggestion that never met a contradicting move is no longer current. */
function staleReason(s: MonitorSnapshot): string {
  if (s.sync_error) return tr('牌局同步异常，已作废')
  if (s.service === 'error') return tr('模型服务异常，已作废')
  if (s.capture.state !== 'running') return tr('采集未运行，已作废')
  if (s.bot.state !== 'ready') return tr('Bot 未就绪，已作废')
  if (s.round_ended) return tr('本局已结束，不再有效')
  return tr('牌局已推进，不再有效')
}

/** The longest position a bot can give and still have it shown: the heading keeps a place of one size for it. */
const POSITION_ROOM = 10

/**
 * What a decision goes by in its heading. A bot can say where the decision
 * falls in the game by its own count (`meta.show.position`) — a step of its
 * model, say — and that is the better number: it is the bot's own, and it shows
 * when the bot has lost its place. Failing that, the monitor's count of the
 * game's decisions. While the answer is still out there is nothing to show:
 * only the answer says which of the two it will be.
 */
export function decisionMark(d: MonitorDecision): string {
  if (d.state === 'thinking') return ''
  const said = (d.response?.meta?.show as { position?: unknown } | undefined)?.position
  const position = typeof said === 'string' ? said.trim() : ''
  return position && position.length <= POSITION_ROOM ? position : `#${d.number}`
}

export function decisionView(s: MonitorSnapshot | null): DecisionView {
  const t = tableOf(s?.game)
  const d = s?.decision ?? null
  if (!s || !d) {
    const message = !s ? tr('正在连接监视数据') : s.game_ended ? tr('等待对局开始') : s.round_ended ? tr('等待下一局开始') : tr('轮到自己时显示模型建议')
    return { phase: 'idle', tone: 'idle', status: tr('待机'), message, live: false, action: null, actual: null, instead: null, trigger: '', steps: ['todo', 'todo', 'todo'] }
  }
  const action = describeAction(d.response, t)
  const after = s.events.filter(e => e.seq > d.context.event_seq)
  const own = d.response ? actorOf(d.response) ?? t.self : t.self
  const move = d.confirmed_seq != null
    ? s.events.find(e => e.seq === d.confirmed_seq)
    : after.find(e => COMMITS.has(e.event.type) && actorOf(e.event) === own)
  const base = { action, actual: null, instead: null, trigger: triggerLabel(d.trigger, t), live: false }
  switch (d.state) {
    case 'thinking':
      return { ...base, phase: 'thinking', tone: 'busy', status: tr('推理中'), message: tr('已向模型发出请求'), action: null, steps: ['active', 'todo', 'todo'] }
    case 'awaiting': {
      const live = !s.round_ended && !s.sync_error && s.service !== 'error'
      return { ...base, phase: 'awaiting', tone: 'wait', status: tr('待确认'), message: tr('等待雀魂的实际动作'), live, steps: ['done', 'done', 'active'] }
    }
    case 'confirmed':
      return { ...base, phase: 'confirmed', tone: 'done', status: tr('已确认'), message: tr('雀魂动作与建议一致'), actual: describeAction(move?.event, t), steps: ['done', 'done', 'done'] }
    case 'declined': {
      const live = !s.round_ended && !s.sync_error && s.service !== 'error'
      return { ...base, phase: 'declined', tone: 'idle', status: tr('不操作'), message: tr('建议过牌，无需确认'), live, steps: ['done', 'done', 'void'] }
    }
    case 'unavailable':
      return { ...base, phase: 'unavailable', tone: 'idle', status: tr('无建议'), message: tr('模型没有返回建议'), action: null, steps: ['done', 'bad', 'void'] }
    case 'failed':
      return { ...base, phase: 'failed', tone: 'bad', status: tr('失败'), message: d.error || tr('模型请求失败'), action: null, steps: ['done', 'bad', 'void'] }
    case 'expired':
      // The backend only attaches an error to an expiry when the game answered
      // this very decision with a different move.
      if (d.error && d.response) {
        return { ...base, phase: 'mismatch', tone: 'bad', status: tr('不一致'), message: tr('雀魂动作与建议不同'), actual: describeAction(move?.event, t), instead: move?.event ?? null, steps: ['done', 'done', 'bad'] }
      }
      return { ...base, phase: 'expired', tone: 'idle', status: tr('已失效'), message: staleReason(s), steps: ['done', d.response ? 'done' : 'void', 'void'] }
  }
}

// ---------------------------------------------------------------- health ----

export type Health = { key: 'capture' | 'bot' | 'service' | 'sync'; name: string; tone: Tone; state: string; detail: string | null }

export function healthOf(s: MonitorSnapshot | null): Health[] {
  const capture: CaptureStatus = s?.capture ?? { state: 'stopped' }
  const bot: BotStatus = s?.bot ?? { state: 'idle' }
  const service = s?.service ?? 'unknown'
  const server: ServerStatus = s?.server ?? { state: 'none' }
  return [
    { key: 'capture', name: tr('采集'), detail: capture.state === 'error' ? capture.error : null,
      ...({ running: { tone: 'ok', state: tr('运行中') }, stopped: { tone: 'idle', state: tr('未运行') }, error: { tone: 'bad', state: tr('异常') } } as const)[capture.state] },
    { key: 'bot', name: 'Bot', detail: bot.state === 'error' ? bot.error : null,
      ...({ ready: { tone: 'ok', state: tr('就绪') }, loading: { tone: 'busy', state: tr('加载中') }, idle: { tone: 'idle', state: tr('未启动') }, stopped: { tone: 'idle', state: tr('已停止') }, error: { tone: 'bad', state: tr('异常') } } as const)[bot.state] },
    // A ready Bot process says nothing about the model behind it. The server
    // not being up comes first; once it is, what the bot reports decides.
    { key: 'service', name: tr('模型'), detail: server.state === 'failed' ? server.error : service === 'error' ? s?.service_error ?? null : null,
      ...(server.state === 'failed' ? { tone: 'bad', state: tr('未运行') } as const
        : server.state === 'installing' ? { tone: 'busy', state: tr('安装中') } as const
        : server.state === 'starting' ? { tone: 'busy', state: tr('加载中') } as const
        : service === 'connected' ? { tone: 'ok', state: tr('已连接') } as const
        : service === 'error' ? { tone: 'bad', state: tr('断开') } as const
        : server.state === 'ready' ? { tone: 'ok', state: tr('就绪') } as const
        : { tone: 'idle', state: tr('待确认') } as const) },
    // Whether the tracked table still matches the game: a gap or a desync voids every suggestion.
    { key: 'sync', name: tr('同步'), detail: s?.sync_error ?? null,
      ...(s?.sync_error ? { tone: 'bad', state: tr('异常') } as const : s?.game && !s.game_ended ? { tone: 'ok', state: tr('正常') } as const : { tone: 'idle', state: tr('无对局') } as const) },
  ]
}

export type Alert = { key: string; title: string; message: string; hint?: string }

/** The four links as one light. Everything here is run by the application
 *  itself, so there is one question — is it all up — and the links tab has the
 *  rest. A fault outranks a link still coming up; a Bot process with no game
 *  to play and a table with no game on it are not faults. */
export function linkOf(health: Health[]): { tone: Tone; summary: string } {
  // A link and its state, as one text: Chinese runs them together, other languages do not.
  const say = (h: Health) => tr('{0}{1}', h.name, h.state)
  const faulty = health.find(h => h.tone === 'bad')
  if (faulty) return { tone: 'bad', summary: say(faulty) }
  const coming = health.find(h => h.tone === 'busy' || h.tone === 'wait')
  if (coming) return { tone: 'busy', summary: say(coming) }
  const down = health.find(h => (h.key === 'capture' || h.key === 'service') && h.tone !== 'ok')
  if (down) return { tone: 'idle', summary: say(down) }
  return { tone: 'ok', summary: tr('全部正常') }
}

export function alertsOf(s: MonitorSnapshot | null, local: { ui: string | null; bridge: string | null }): Alert[] {
  const out: Alert[] = []
  if (local.bridge) out.push({ key: 'bridge', title: tr('监视数据通道异常'), message: local.bridge })
  if (local.ui) out.push({ key: 'ui', title: tr('窗口操作失败'), message: local.ui })
  if (!s) return out
  if (s.capture.state === 'error') out.push({ key: 'capture', title: tr('采集异常'), message: s.capture.error })
  if (s.bot.state === 'error') out.push({ key: 'bot', title: tr('Bot 进程异常'), message: s.bot.error })
  if (s.server.state === 'failed') out.push({ key: 'server', title: tr('模型服务未运行'), message: s.server.error, hint: tr('应用会自动重新启动它。') })
  else if (s.service === 'error') out.push({ key: 'service', title: tr('模型服务断开'), message: s.service_error ?? tr('模型服务没有应答。') })
  if (s.sync_error) out.push({ key: 'sync', title: tr('牌局同步异常'), message: s.sync_error, hint: tr('当前建议已作废，下一局开始后恢复。') })
  return out
}

/** The bot's service in a sentence: what it is doing as a process the app
 *  runs, under the name it gave itself. */
/** The model service's state in words. `named`: with the model's name, for a
 *  place that does not show it on a line of its own. */
export function serverLine(s: MonitorSnapshot | null, now: number, named = true): string {
  const server: ServerStatus = s?.server ?? { state: 'none' }
  const seconds = (ms: number) => tr('{0} 秒', Math.max(0, Math.round(ms / 1000)))
  switch (server.state) {
    case 'installing': return tr('正在安装运行环境，首次需要下载依赖')
    case 'starting': return tr('已启动 {0}，正在加载', seconds(now - server.since_ms))
    case 'failed': return tr('{0}后重新启动', seconds(server.retry_at_ms - now))
    case 'ready': return named ? tr('{0} · 应用托管 · 启动 {1}', server.label ?? server.bot, seconds(server.startup_ms)) : tr('应用托管 · 启动 {0}', seconds(server.startup_ms))
    case 'none': return tr('收到第一份候选后确认，与 Bot 进程状态无关')
  }
}

/** What the bot and its service say about themselves, the bot's latest reply
 *  first. There is room for `limit` of them. */
export function factsOf(s: MonitorSnapshot | null, limit = 3): Fact[] {
  const server: ServerStatus = s?.server ?? { state: 'none' }
  return [...(s?.facts ?? []), ...(server.state === 'ready' ? server.facts : [])].slice(0, limit)
}

// --------------------------------------------------------------- results ----

const DRAWS: Record<string, string> = localized({
  exhaustive: '荒牌流局', nagashi: '流局满贯', kyuushu: '九种九牌', four_winds: '四风连打',
  four_kans: '四杠散了', four_riichis: '四家立直', draw: '流局',
})
export function resultTitle(r: RoundResult): string {
  return r.kind === 'hora' ? tr('和牌') : DRAWS[r.kind] ?? tr('流局')
}
/** How a publicly revealed hand ended the round. A revealed hand at a draw is
 *  only that — a reveal; an unrevealed seat is not thereby "noten". */
export function handOutcome(h: PublicHand, t: Table): string {
  if (h.target == null) return tr('公开手牌')
  return h.target === h.seat ? tr('自摸') : tr('荣和 · {0}放铳', seatLabel(h.target, t))
}
export function signed(n: number): string {
  return `${n > 0 ? '+' : n < 0 ? '−' : '±'}${new Intl.NumberFormat('en-US').format(Math.abs(n))}`
}

// ---------------------------------------------------------------- events ----

export type EventLine = { seat: number | null; text: string; faces: Face[]; quiet: boolean }

export function eventLine(e: MjaiEvent, t: Table): EventLine {
  const seat = actorOf(e)
  switch (e.type) {
    case 'start_game': return { seat: null, text: tr('对局开始'), faces: [], quiet: false }
    case 'start_kyoku': return { seat: null, text: tr('{0}{1}局 开始 · {2} 本场', WIND[e.bakaze] ?? e.bakaze, e.kyoku, e.honba), faces: [], quiet: false }
    case 'end_kyoku': return { seat: null, text: tr('本局结束'), faces: [], quiet: false }
    case 'end_game': return { seat: null, text: tr('对局结束'), faces: [], quiet: false }
    case 'tsumo': return { seat, text: tr('摸牌'), faces: e.pai === '?' ? [] : [{ tile: e.pai }], quiet: true }
    case 'dahai': return { seat, text: e.tsumogiri ? tr('摸切') : tr('手切'), faces: [{ tile: e.pai }], quiet: false }
    case 'reach': return { seat, text: tr('宣告立直'), faces: [], quiet: false }
    case 'reach_accepted': return { seat, text: tr('立直成立'), faces: [], quiet: true }
    case 'dora': return { seat: null, text: tr('新宝牌指示'), faces: [{ tile: e.dora_marker }], quiet: false }
    case 'ryukyoku': return { seat: null, text: DRAWS[e.reason ?? 'draw'] ?? tr('流局'), faces: [], quiet: false }
    case 'none': return { seat: null, text: tr('过'), faces: [], quiet: true }
    default: {
      const view = describeAction(e, t)
      return { seat, text: view?.verb ?? e.type, faces: view?.faces ?? [], quiet: false }
    }
  }
}

// ----------------------------------------------------------------- table ----

/** Own concealed hand, sorted, with the fresh draw pulled out to stand apart. */
export function ownHand(game: GameStateSnapshot): { tiles: string[]; drawn: string | null } {
  const me = game.our_seat == null ? null : game.players[game.our_seat]
  if (!me) return { tiles: [], drawn: null }
  const tiles = sorted(me.tehai.filter(tile => tile && tile !== '?'))
  const drawn = me.drawn_tile ?? null
  const at = drawn ? tiles.lastIndexOf(drawn) : -1
  if (at < 0) return { tiles, drawn: null }
  tiles.splice(at, 1)
  return { tiles, drawn }
}

/** Which tiles of the hand a live suggestion would take out of it. */
export function handMarks(hand: { tiles: string[]; drawn: string | null }, response: MjaiEvent | null): { tiles: Set<number>; drawn: boolean } {
  const marks = { tiles: new Set<number>(), drawn: false }
  if (!response) return marks
  const take = (tile: string, preferDrawn = false) => {
    if (preferDrawn && hand.drawn === tile) { marks.drawn = true; return }
    const at = hand.tiles.findIndex((own, i) => own === tile && !marks.tiles.has(i))
    if (at >= 0) marks.tiles.add(at)
    else if (hand.drawn === tile) marks.drawn = true
  }
  if (response.type === 'dahai') take(response.pai, response.tsumogiri)
  else if (response.type === 'reach' && response.pai) take(response.pai)
  else if (response.type === 'kakan') take(response.pai, true)
  else if ('consumed' in response) response.consumed.forEach(tile => take(tile))
  return marks
}

/** A meld drawn from the tracker's own record, so a red five keeps its place. */
export function meldView(m: MeldSnapshot, seat: number, t: Table): Face[] {
  if (m.kind === 'ankan') return concealedKan(m.tiles)
  const called = m.called_tile ?? m.tiles[0] ?? '?'
  const own = [...m.tiles]
  const at = own.indexOf(called)
  if (at >= 0) own.splice(at, 1)
  if (m.kind !== 'kakan') return meldFaces(called, own, m.from_who, seat, t)
  // The added tile joins the claimed one, both lying down. Which of the three
  // remaining tiles was added is not recorded; a red five is kept upright so it
  // stays readable.
  const added = own.findIndex(tile => !tile.endsWith('r'))
  const [extra] = own.splice(added < 0 ? 0 : added, 1)
  const faces = meldFaces(called, own, m.from_who, seat, t)
  faces.splice(faces.findIndex(f => f.side) + 1, 0, { tile: extra ?? called, side: true })
  return faces
}
