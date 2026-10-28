import { beforeEach, describe, expect, it } from 'vitest'
import { figuresOf, rankPointChange, roomName, roomOf, totalsOf, yakuName, type GameRecord } from './history'
import { setLang } from './i18n'

const game = (go_type: number, lobby = 0, num_players = 4): GameRecord => ({
  id: 'record', ended_at: '2026-10-27T00:00:00Z', platform: 'tenhou', num_players, our_seat: 0,
  final_scores: [], our_rank: 1, stats: {}, match_info: { platform: 'tenhou', go_type, lobby },
  stats_complete: true, stats_version: 1, yaku_wins: 0,
})
beforeEach(() => setLang('zh-CN'))

describe('statistics sample coverage', () => {
  it('keeps the two providers yaku ids separate, including Tenhou id zero', () => {
    const native = { ...game(1), stats: { round: 4, agari: 2 }, yaku: { 0: 1, 1: 2 }, yaku_wins: 2 }
    const soul = { ...native, platform: 'majsoul', yaku: { 1: 1 }, yaku_wins: 1 }
    const totals = totalsOf([native, soul])
    expect(totals.yaku).toContainEqual({ platform: 'tenhou', id: 0, wins: 1 })
    expect(totals.yaku).toContainEqual({ platform: 'majsoul', id: 1, wins: 1 })
    expect(yakuName(0, 'tenhou')).toBe('门前清自摸和')
    expect(yakuName(1, 'tenhou')).toBe('立直')
    expect(yakuName(1, 'majsoul')).toBe('门前清自摸和')
    expect(yakuName(999, 'tenhou')).toContain('999')
    expect([totals.scoredWins, totals.wins]).toEqual([3, 4])
  })
  it('does not invent sample coverage from legacy aggregate yaku', () => {
    const unknown = { ...game(1), stats: { round: 4, agari: 2 }, yaku: { 1: 2 }, yaku_wins: null }
    const known = { ...game(1), stats: { round: 4, agari: 1 }, yaku: { 1: 1 }, yaku_wins: 1 }
    const totals = totalsOf([unknown, known])
    expect([totals.scoredWins, totals.wins]).toEqual([1, 3])
    expect(totals.yaku).toEqual([{ platform: 'tenhou', id: 1, wins: 1 }])
    expect(figuresOf(totals).find(f => f.name === '役满')?.value).toBe('—')
  })
  it('excludes unfinished hands only from detailed samples, preserving final placement and pt', () => {
    const complete = { ...game(1), stats: { round: 4, agari: 1, houjuu: 1, houjuu_point_to_oya: -8000 }, our_rank_points: 20 }
    const broken = { ...complete, stats_complete: false, stats: { round: 10, agari: 9 }, our_rank: 4 }
    const totals = totalsOf([complete, broken, { ...broken, our_rank: null }])
    expect([totals.games, totals.detailGames, totals.placementGames, totals.hands]).toEqual([3, 1, 2, 4])
    expect(totals.places).toEqual([1, 0, 0, 1])
    expect(totals.averagePlace).toBe(2.5)
    expect(figuresOf(totals).find(f => f.name === '和了率')?.value).toBe('25.0%')
    expect(figuresOf(totals).find(f => f.name === '平均放铳点')?.value).toBe('8,000')
    expect(figuresOf(totalsOf([broken])).every(f => f.value === '—')).toBe(true)
  })
})

describe('the platform room on a stored game', () => {
  it('keeps Tenhou general East distinct from Mahjong Soul mode 1', () => {
    const tenhou = game(1)
    const majsoul: GameRecord = { ...tenhou, platform: 'majsoul', match_info: { platform: 'majsoul', mode_id: 1 } }
    expect(roomName(roomOf(tenhou))).toBe('一般·东风')
    expect(roomName(roomOf(majsoul))).toBe('铜之间')
  })
  it.each([[9, '一般·东南'], [129, '上级·东风'], [137, '上级·东南'], [33, '特上·东风'], [41, '特上·东南'], [161, '凤凰·东风'], [169, '凤凰·东南']] as const)('reads the declared Tenhou tier and length from GO %i', (rules, name) => {
    expect(roomName(roomOf(game(rules)))).toBe(name)
  })
  it('separates sanma and actual private lobbies, and does not guess absent metadata', () => {
    expect(roomName(roomOf(game(17, 0, 3)))).toBe('三麻一般·东风')
    expect(roomName(roomOf(game(1, 1234)))).toBe('天凤个室')
    expect(roomName(roomOf({ ...game(1), match_info: null }))).toBe('未记录房间')
    expect(roomName(roomOf({ ...game(1), match_info: { platform: 'tenhou', go_type: 1 } }))).toBe('未记录房间')
    expect(roomName(roomOf(game(17)))).toBe('其他房间')
  })
})

describe('Tenhou per-game pt under the published ladder rules', () => {
  const ranked = (rules: number, dan: number, rank: number, players = 4): GameRecord => ({
    ...game(rules, 0, players), our_rank: rank, match_info: { platform: 'tenhou', go_type: rules, lobby: 0, dan },
  })
  // Published table at https://tenhou.net/man/ (段級位制), verified 2026-10-27.
  it.each([
    [1, 4, [20, 10, 0, -30]], [9, 4, [30, 15, 0, -45]],
    [129, 4, [40, 10, 0, -30]], [137, 4, [60, 15, 0, -45]],
    [33, 4, [50, 20, 0, -30]], [41, 4, [75, 30, 0, -45]],
    [161, 4, [60, 30, 0, -30]], [169, 4, [90, 45, 0, -45]],
    [17, 3, [30, 0, -30]], [25, 3, [45, 0, -45]],
    [145, 3, [50, 0, -30]], [153, 3, [75, 0, -45]],
    [49, 3, [70, 0, -30]], [57, 3, [105, 0, -45]],
    [177, 3, [90, 0, -30]], [185, 3, [135, 0, -45]],
  ] as const)('uses every placement in GO %i with %i players', (rules, players, expected) => {
    expect(expected.map((_, index) => rankPointChange(ranked(rules, 10, index + 1, players)))).toEqual(expected)
  })
  it('uses the rank at the start, including kyu and dan loss boundaries', () => {
    const ranks = [0, 7, 8, 9, 10, 11, 19]
    expect(ranks.map(dan => rankPointChange(ranked(1, dan, 4)))).toEqual([0, 0, -10, -20, -30, -40, -120])
    expect(ranks.map(dan => rankPointChange(ranked(25, dan, 3, 3)))).toEqual([0, 0, -15, -30, -45, -60, -180])
  })
  it('keeps overtime East and cumulative awards independent of promotion balance resets', () => {
    const overtime = { ...ranked(1, 9, 1), kyoku_mode: 'east_south' }
    expect(rankPointChange(overtime)).toBe(20)
    const sequence = [ranked(129, 9, 1), ranked(129, 10, 3), ranked(129, 10, 4)]
    expect(totalsOf(sequence).points).toEqual([40, 40, 10])
  })
  it('keeps missing native fields, observers and unpublished Tenhoui pt uncomputed', () => {
    const full = ranked(1, 8, 1)
    const incomplete: GameRecord[] = [
      { ...full, our_rank: null }, { ...full, our_rank: 5 }, { ...full, match_info: null },
      { ...full, match_info: { platform: 'tenhou', go_type: 1, lobby: 0 } },
      { ...full, match_info: { platform: 'tenhou', go_type: 1, dan: 8 } },
      ranked(1, 20, 1), ranked(1, 21, 1), ranked(257, 8, 1),
    ]
    expect(incomplete.map(rankPointChange)).toEqual(incomplete.map(() => null))
    const totals = totalsOf([full, ...incomplete, ranked(1, 8, 4)])
    expect(totals.pointGames).toBe(2)
    expect(totals.points).toEqual([20, 10])
  })
  it('keeps private/computer games at zero and preserves other platforms reported values', () => {
    expect(rankPointChange(game(1, 1234))).toBe(0)
    expect(rankPointChange(game(0))).toBe(0)
    expect(rankPointChange({ ...game(1), platform: 'majsoul', match_info: { platform: 'majsoul', mode_id: 9 }, our_rank_points: -165 })).toBe(-165)
  })
})
