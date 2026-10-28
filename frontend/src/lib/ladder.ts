// Where the player stands on the platform's rank ladder, as the platform says
// it: the dock names the rank and shows its points, and works nothing out.

import { localized, tr } from '@/lib/i18n'

/** A place on a rank ladder: on Mahjong Soul `id` is the ladder (1 four-player,
 *  2 three-player), the rank and its star — 10303 is four-player, the third
 *  rank, third star — and `score` the points held toward the next star.
 *  On Tenhou `id` is the native PF rank (0 newcomer, 10 shodan, 11 nidan)
 *  and `score` is its current server-reported pt balance. */
export type Level = { id: number; score: number }
/** The player's own ranks as the platform last said them (at login, and after
 *  every game), and when. */
export type AccountLevels = { platform: 'majsoul' | 'tenhou'; level: Level | null; level3: Level | null; at: string }

const RANK: Record<number, string> = localized({ 1: '初心', 2: '雀士', 3: '雀杰', 4: '雀豪', 5: '雀圣', 6: '魂天', 7: '魂天' })
const STAR: Record<number, string> = localized({ 1: '一', 2: '二', 3: '三' })
/** The points that make the next star, by rank and star: the number the client
 *  shows under its badge (雀杰二 1227/1400). Above 雀圣 the client counts otherwise. */
const CEILING: Record<number, readonly [number, number, number]> = {
  1: [20, 80, 200], 2: [600, 800, 1000], 3: [1200, 1400, 2000], 4: [2800, 3200, 3600], 5: [4000, 6000, 9000],
}

const TENHOU_RANK: Record<number, string> = localized({
  0: '新人', 1: '九级', 2: '八级', 3: '七级', 4: '六级', 5: '五级', 6: '四级', 7: '三级', 8: '二级', 9: '一级',
  10: '初段', 11: '二段', 12: '三段', 13: '四段', 14: '五段', 15: '六段', 16: '七段', 17: '八段', 18: '九段', 19: '十段', 20: '天凤位',
})
// Official four/three-player promotion table, verified 2026-10-27:
// https://tenhou.net/man/#RANK . Balances themselves always come from PF4/PF3.
const TENHOU_CEILING = [20, 20, 20, 20, 40, 60, 80, 100, 100, 100, 400, 800, 1200, 1600, 2000, 2400, 2800, 3200, 3600, 4000] as const

/** A rank as shown: its name (雀杰三), its points, and the points of the next
 *  star where there is one. */
export type Rung = { name: string; score: number; ceiling: number | null }

export function rungOf(level: Level, platform: AccountLevels['platform']): Rung {
  if (platform === 'tenhou') return {
    name: TENHOU_RANK[level.id] ?? tr('段位 #{0}', level.id),
    score: level.score,
    ceiling: TENHOU_CEILING[level.id] ?? null,
  }
  const rank = Math.floor(level.id / 100) % 100
  const star = level.id % 100
  const name = !RANK[rank] ? tr('段位 #{0}', level.id)
    : rank >= 6 ? tr('{0} Lv{1}', RANK[rank], star)
    : STAR[star] ? tr('{0}{1}', RANK[rank], STAR[star])
    : RANK[rank]
  return { name, score: level.score, ceiling: CEILING[rank]?.[star - 1] ?? null }
}
