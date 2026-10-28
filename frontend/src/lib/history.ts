import { localized, tr } from '@/lib/i18n'
// The games on record (`get_history`) and what the statistics tab works out
// from them. The core stores one line a game: how it ended for us, a block of
// counters for the hands in it, and — from the builds that keep them — what it
// did to reported rank points and which yaku our wins had. Tenhou's per-game
// pt award is calculated from the recorded GO/UN and final placement below.

/** A game as the core recorded it. Only what the tab reads is named. */
export type GameRecord = {
  id: string
  ended_at: string
  platform: string
  num_players: number
  our_seat: number | null
  final_scores: number[]
  our_rank: number | null
  /** Per-hand counters, from our seat: `round`, `agari`, `houjuu`, `riichi`, … */
  stats: Record<string, number>
  match_info?:
    | { platform: 'majsoul'; mode_id?: number; room_id?: number; contest_uid?: number }
    | { platform: 'tenhou'; go_type?: number; lobby?: number; dan?: number; log_id?: string }
    | { platform: 'riichi_city'; stage_type?: number; game_play?: number }
    | null
  /** `null` or missing on records from before it was kept. */
  our_rank_points?: number | null
  /** Yaku id → how many of our winning hands this game had it. */
  yaku?: Record<string, number>
  yaku_wins: number | null
  stats_complete: boolean
  stats_version: number
}

// ----------------------------------------------------------------- rooms ----

/** Mahjong Soul's ranked match modes by room, from the game's own table:
 *  three four-player modes a room below Throne, two three-player ones. */
type MajsoulRoom = 'bronze' | 'silver' | 'gold' | 'jade' | 'throne' | 'other'
type TenhouTier = 'ippan' | 'joukyu' | 'tokujou' | 'houou'
type TenhouLength = 'east' | 'south'
export type Room = MajsoulRoom | `tenhou:${TenhouTier}:${TenhouLength}:${3 | 4}` | 'tenhou:private' | 'unrecorded' | 'unsupported'
const TIER_OF_MODE: Record<number, MajsoulRoom> = {
  1: 'bronze', 2: 'bronze', 3: 'bronze', 4: 'silver', 5: 'silver', 6: 'silver', 7: 'gold', 8: 'gold', 9: 'gold',
  10: 'jade', 11: 'jade', 12: 'jade', 15: 'throne', 16: 'throne',
  17: 'bronze', 18: 'bronze', 19: 'silver', 20: 'silver', 21: 'gold', 22: 'gold', 23: 'jade', 24: 'jade', 25: 'throne', 26: 'throne',
}
const TENHOU_TIERS = ['ippan', 'joukyu', 'tokujou', 'houou'] as const
const TENHOU_TIER = localized({ ippan: '一般', joukyu: '上级', tokujou: '特上', houou: '凤凰' })
const TENHOU_LENGTH = localized({ east: '东风', south: '东南' })
const OTHER_ROOM_NAMES = localized({ bronze: '铜之间', silver: '银之间', gold: '金之间', jade: '玉之间', throne: '王座之间', other: '友人场及其他', 'tenhou:private': '天凤个室', unrecorded: '未记录房间', unsupported: '其他房间' })
export const ROOMS: Room[] = [
  'bronze', 'silver', 'gold', 'jade', 'throne', 'other',
  ...TENHOU_TIERS.flatMap(tier => ([4, 3] as const).flatMap(players => (['east', 'south'] as const).map(length => `tenhou:${tier}:${length}:${players}` as Room))),
  'tenhou:private', 'unrecorded', 'unsupported',
]

export function roomName(room: Room): string {
  if (room in OTHER_ROOM_NAMES) return OTHER_ROOM_NAMES[room as keyof typeof OTHER_ROOM_NAMES]
  const [, tier, length, players] = room.split(':') as ['tenhou', TenhouTier, TenhouLength, '3' | '4']
  const name = tr('{0}·{1}', TENHOU_TIER[tier], TENHOU_LENGTH[length])
  return players === '3' ? tr('三麻{0}', name) : name
}

type TenhouRoom =
  | { kind: 'unrecorded' | 'unsupported' | 'private' | 'unranked' }
  | { kind: 'ranked'; tierIndex: 0 | 1 | 2 | 3; tier: TenhouTier; length: TenhouLength; players: 3 | 4; dan: number | null }

/** One normalization of the recorded native room; neither labels nor pt math
 *  infer missing GO/UN fields from final scores or an overtime wind. */
function tenhouRoom(r: GameRecord): TenhouRoom {
  const info = r.match_info
  if (r.platform !== 'tenhou' || info?.platform !== 'tenhou') return { kind: 'unsupported' }
  if (info.lobby == null) return { kind: 'unrecorded' }
  if (!Number.isInteger(info.lobby) || info.lobby < 0 || info.lobby > 0xffffffff) return { kind: 'unsupported' }
  if (info.lobby !== 0) return { kind: 'private' }
  const rules = info.go_type
  if (rules == null) return { kind: 'unrecorded' }
  if (!Number.isInteger(rules) || rules < 0 || rules > 0xff) return { kind: 'unsupported' }
  const players = (rules & 0x10) !== 0 ? 3 : 4
  if (players !== r.num_players) return { kind: 'unsupported' }
  if ((rules & 1) === 0) return { kind: 'unranked' }
  // Official client's room index, also used by the serving adapter's pt table.
  const tierIndex = (((rules & 0x20) >> 4) | ((rules & 0x80) >> 7)) as 0 | 1 | 2 | 3
  const dan = info.dan != null && Number.isInteger(info.dan) && info.dan >= 0 && info.dan <= 20 ? info.dan : null
  return { kind: 'ranked', tierIndex, tier: TENHOU_TIERS[tierIndex], length: (rules & 0x08) !== 0 ? 'south' : 'east', players, dan }
}

/** Classify only the platform's recorded declaration. Tenhou GO bit 0x08 is
 *  the scheduled length: an east game that extends into South is still east.
 *  Unknown or absent metadata never becomes a Mahjong Soul friendly room. */
export function roomOf(r: GameRecord): Room {
  const info = r.match_info
  if (!info) return 'unrecorded'
  if (info.platform !== r.platform) return 'unsupported'
  if (info.platform === 'majsoul') {
    if (info.mode_id != null) return TIER_OF_MODE[info.mode_id] ?? 'other'
    return info.room_id != null || info.contest_uid != null ? 'other' : 'unrecorded'
  }
  if (info.platform !== 'tenhou') return 'unsupported'
  const room = tenhouRoom(r)
  switch (room.kind) {
    case 'ranked': return `tenhou:${room.tier}:${room.length}:${room.players}`
    case 'private': return 'tenhou:private'
    case 'unranked': return 'unsupported'
    default: return room.kind
  }
}

/** Per-game pt award under Tenhou's published ladder table, verified 2026-10-27:
 *  https://tenhou.net/man/ (段級位制). This is a rule calculation, not a change
 *  in the account's displayed balance after promotion/demotion resets.
 *  get_history contains finalized Confirmed games; its recorder discards
 *  Terminated games. A missing own placement or UN rank remains uncomputed. */
export function rankPointChange(r: GameRecord): number | null {
  if (r.platform !== 'tenhou') return r.our_rank_points != null && Number.isFinite(r.our_rank_points) ? r.our_rank_points : null
  if ((r.num_players !== 3 && r.num_players !== 4) || r.our_rank == null || !Number.isInteger(r.our_rank) || r.our_rank < 1 || r.our_rank > r.num_players) return null
  const room = tenhouRoom(r)
  if (room.kind === 'private' || room.kind === 'unranked') return 0
  if (room.kind !== 'ranked' || room.dan == null || room.dan === 20) return null // no published Tenhoui pt table
  const first = (room.players === 4 ? [20, 40, 50, 60] : [30, 50, 70, 90])[room.tierIndex]
  const second = room.players === 4 ? [10, 10, 20, 30][room.tierIndex] : 0
  const last = room.dan > 7 ? -(room.dan - 7) * 10 : 0
  const awards = room.players === 4 ? [first, second, 0, last] : [first, second, last]
  return awards[r.our_rank - 1] * (room.length === 'south' ? 1.5 : 1)
}

// ------------------------------------------------------------------ yaku ----

/** Mahjong Soul's ids for yaku. An id that is not here is shown by its number. */
const YAKU_NAME: Record<number, string> = localized({
  1: '门前清自摸和', 2: '立直', 3: '抢杠', 4: '岭上开花', 5: '海底摸月', 6: '河底捞鱼', 7: '役牌 白', 8: '役牌 发', 9: '役牌 中',
  10: '役牌 自风', 11: '役牌 场风', 12: '断幺九', 13: '一杯口', 14: '平和', 15: '混全带幺九', 16: '一气通贯', 17: '三色同顺',
  18: '两立直', 19: '三色同刻', 20: '三杠子', 21: '对对和', 22: '三暗刻', 23: '小三元', 24: '混老头', 25: '七对子',
  26: '纯全带幺九', 27: '混一色', 28: '二杯口', 29: '清一色', 30: '一发', 31: '宝牌', 32: '红宝牌', 33: '里宝牌', 34: '拔北宝牌',
  35: '天和', 36: '地和', 37: '大三元', 38: '四暗刻', 39: '字一色', 40: '绿一色', 41: '清老头', 42: '国士无双', 43: '小四喜',
  44: '四杠子', 45: '九莲宝灯', 46: '八连庄', 47: '纯正九莲宝灯', 48: '四暗刻单骑', 49: '国士无双十三面', 50: '大四喜',
})
// Tenhou's ids are a different native namespace, including id 0 for tsumo.
const TENHOU_YAKU_NAME: Record<number, string> = localized({
  0: '门前清自摸和', 1: '立直', 2: '一发', 3: '抢杠', 4: '岭上开花', 5: '海底摸月', 6: '河底捞鱼',
  7: '平和', 8: '断幺九', 9: '一杯口', 10: '自风 东', 11: '自风 南', 12: '自风 西', 13: '自风 北',
  14: '场风 东', 15: '场风 南', 16: '场风 西', 17: '场风 北', 18: '役牌 白', 19: '役牌 发', 20: '役牌 中',
  21: '两立直', 22: '七对子', 23: '混全带幺九', 24: '一气通贯', 25: '三色同顺', 26: '三色同刻',
  27: '三杠子', 28: '对对和', 29: '三暗刻', 30: '小三元', 31: '混老头', 32: '二杯口', 33: '纯全带幺九',
  34: '混一色', 35: '清一色', 36: '人和', 37: '天和', 38: '地和', 39: '大三元', 40: '四暗刻',
  41: '四暗刻单骑', 42: '字一色', 43: '绿一色', 44: '清老头', 45: '九莲宝灯', 46: '纯正九莲宝灯',
  47: '国士无双', 48: '国士无双十三面', 49: '大四喜', 50: '小四喜', 51: '四杠子', 52: '宝牌', 53: '里宝牌', 54: '红宝牌',
})
export const yakuName = (id: number, platform: string): string =>
  (platform === 'tenhou' ? TENHOU_YAKU_NAME : platform === 'majsoul' ? YAKU_NAME : {})[id] ?? tr('役 #{0}', id)

// ----------------------------------------------------------------- totals ----

export type Totals = {
  games: number
  placementGames: number
  detailGames: number
  /** Hands played. */
  hands: number
  /** Games finished in each place, first to last. */
  places: number[]
  averagePlace: number | null
  /** Cumulative per-game pt changes, oldest first; excludes ladder balance resets. */
  points: number[]
  /** Games with a reported or fully computable per-game pt change. */
  pointGames: number
  /** The counters of every game added up. */
  sum: Record<string, number>
  /** Yaku by how many of our wins had it, most first. */
  yaku: Array<{ platform: string; id: number; wins: number }>
  /** Our wins in the games that say how a hand was scored. */
  scoredWins: number
  wins: number
}

/** Add up `records`, which are taken in the order given (oldest first, as the core lists them). */
export function totalsOf(records: GameRecord[]): Totals {
  const players = records.some(r => r.num_players === 4) || records.length === 0 ? 4 : 3
  const places = new Array<number>(players).fill(0)
  const sum: Record<string, number> = {}
  const yaku = new Map<string, { platform: string; id: number; wins: number }>()
  const points: number[] = []
  let placed = 0, placeSum = 0, scoredWins = 0, detailGames = 0, wins = 0
  for (const r of records) {
    if (r.our_rank != null && Number.isInteger(r.our_rank) && r.our_rank >= 1 && r.our_rank <= r.num_players) { places[r.our_rank - 1] += 1; placed += 1; placeSum += r.our_rank }
    const change = rankPointChange(r)
    if (change != null) points.push((points.at(-1) ?? 0) + change)
    if (r.stats_complete) {
      detailGames++
      for (const [key, value] of Object.entries(r.stats)) sum[key] = (sum[key] ?? 0) + value
    }
    // A known winning result can retain its yaku even if another hand in
    // that game is missing. The detailed rate sample is independently gated.
    wins += r.stats.agari ?? 0
    const had = Object.entries(r.yaku ?? {})
    // Legacy aggregates with unknown sample coverage stay on disk but cannot
    // be mixed into a frequency denominator for verified winning hands.
    if (r.yaku_wins == null) continue
    scoredWins += r.yaku_wins
    for (const [id, wins] of had) {
      const key = `${r.platform}:${id}`, old = yaku.get(key)
      yaku.set(key, { platform: r.platform, id: Number(id), wins: (old?.wins ?? 0) + wins })
    }
  }
  return {
    games: records.length, placementGames: placed, detailGames, hands: sum.round ?? 0, places, averagePlace: placed ? placeSum / placed : null,
    points, pointGames: points.length, sum,
    yaku: [...yaku.values()].sort((a, b) => b.wins - a.wins || a.platform.localeCompare(b.platform) || a.id - b.id),
    scoredWins, wins,
  }
}

/** One line of the figures column: a name and a value already written out. */
export type Figure = { name: string; value: string; note?: string }

const percent = (part: number, whole: number): string => (whole > 0 ? `${(100 * part / whole).toFixed(1)}%` : '—')
const average = (total: number, count: number, digits = 0): string =>
  count > 0 ? (total / count).toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits }) : '—'

/** The figures a riichi player looks for first, in the order they are usually read. */
export function figuresOf(t: Totals): Figure[] {
  const s = (key: string) => t.sum[key] ?? 0
  const figures = [
    { name: tr('和了率'), value: percent(s('agari'), t.hands), note: tr('和了的局数 ÷ 总局数') },
    { name: tr('放铳率'), value: percent(s('houjuu'), t.hands), note: tr('放铳的局数 ÷ 总局数') },
    { name: tr('立直率'), value: percent(s('riichi'), t.hands) },
    { name: tr('副露率'), value: percent(s('fuuro'), t.hands) },
    { name: tr('流局率'), value: percent(s('ryukyoku'), t.hands) },
    { name: tr('平均和了点'), value: average(s('agari_point_oya') + s('agari_point_ko'), s('agari')) },
    { name: tr('平均放铳点'), value: average(-(s('houjuu_point_to_oya') + s('houjuu_point_to_ko') + s('houjuu_point_unattributed')), s('houjuu')) },
    { name: tr('和了巡数'), value: average(s('agari_jun'), s('agari'), 1) },
    { name: tr('立直巡数'), value: average(s('riichi_jun'), s('riichi'), 1) },
    { name: tr('立直和了率'), value: percent(s('riichi_agari'), s('riichi')), note: tr('立直后和了 ÷ 立直次数') },
    { name: tr('立直放铳率'), value: percent(s('riichi_houjuu'), s('riichi')), note: tr('立直后放铳 ÷ 立直次数') },
    { name: tr('副露和了率'), value: percent(s('fuuro_agari'), s('fuuro')), note: tr('副露后和了 ÷ 副露的局数') },
    { name: tr('副露放铳率'), value: percent(s('fuuro_houjuu'), s('fuuro')), note: tr('副露后放铳 ÷ 副露的局数') },
    { name: tr('默听和了'), value: percent(s('dama_agari'), s('agari')), note: tr('门前不立直的和了 ÷ 和了次数') },
    { name: tr('追立'), value: tr('{0} 次', s('chasing_riichi')), note: tr('在别家立直之后宣告的立直') },
    { name: tr('役满'), value: t.scoredWins === t.wins ? tr('{0} 次', s('yakuman')) : '—' },
    { name: tr('总局数'), value: tr('{0} 局', t.hands) },
  ]
  return t.detailGames > 0 ? figures : figures.map(figure => ({ ...figure, value: '—' }))
}
