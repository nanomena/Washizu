import { afterEach, describe, expect, it } from 'vitest'
import { setLang } from '@/lib/i18n'
import { CAPABILITIES, DEFAULT_PLATFORMS, PLATFORMS, capsOf, platformName } from '@/lib/platform'
import { roomOf, yakuName, type GameRecord } from '@/lib/history'

describe('the platforms', () => {
  afterEach(() => setLang('zh-CN'))

  it('says what each offers in one table, and HoloMahjong only shows its window', () => {
    expect(PLATFORMS).toEqual(['Majsoul', 'Tenhou', 'HoloMahjong'])
    expect(Object.keys(CAPABILITIES).sort()).toEqual([...PLATFORMS].sort())
    expect(CAPABILITIES.HoloMahjong).toEqual({ view: 'mirror', autoplay: false, clientAutoWin: 'none', obsOutput: false, gameProxy: false, captureListen: true })
    expect(CAPABILITIES.Majsoul).toEqual({ view: 'page', autoplay: true, clientAutoWin: 'switch', obsOutput: true, gameProxy: true, captureListen: false })
    expect(CAPABILITIES.Tenhou).toEqual({ view: 'page', autoplay: true, clientAutoWin: 'unadapted', obsOutput: true, gameProxy: true, captureListen: false })
    expect(capsOf(undefined)).toBeNull()
    expect(DEFAULT_PLATFORMS).toEqual(PLATFORMS.filter(platform => CAPABILITIES[platform].autoplay))
  })

  it.each([
    ['zh-CN', ['雀魂', '天凤', 'HoloM']], ['zh-TW', ['雀魂', '天鳳', 'HoloM']],
    ['ja', ['雀魂', '天鳳', 'HoloM']], ['en', ['Mahjong Soul', 'Tenhou', 'HoloM']],
  ] as const)('names every platform as itself in %s', (lang, names) => {
    setLang(lang)
    expect(PLATFORMS.map(platformName)).toEqual(names)
  })

  it('never files a HoloMahjong game under another platform\'s rooms or yaku', () => {
    const record: GameRecord = { id: 'G', ended_at: '2026-01-01T00:00:00Z', platform: 'holo_mahjong', num_players: 4, our_seat: 0,
      final_scores: [25000, 25000, 25000, 25000], our_rank: 1, stats: {}, yaku_wins: 1, stats_complete: true, stats_version: 1 }
    expect(roomOf(record)).toBe('unrecorded')
    expect(roomOf({ ...record, match_info: { platform: 'majsoul', mode_id: 9 } })).toBe('unsupported')
    expect(yakuName(1, 'holo_mahjong')).toBe('役 #1')
  })
})
