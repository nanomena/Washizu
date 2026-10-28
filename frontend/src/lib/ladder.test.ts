import { afterEach, describe, expect, it } from 'vitest'
import { rungOf } from '@/lib/ladder'
import { setLang } from '@/lib/i18n'

describe('a rank as the platform gives it', () => {
  afterEach(() => setLang('zh-CN'))

  it('is named by its rank and star, with the points of the next star', () => {
    // As the client showed them: 雀杰二 1227/1400, then 雀杰三 from 1000 after the step up.
    expect(rungOf({ id: 10302, score: 1227 }, 'majsoul')).toEqual({ name: '雀杰二', score: 1227, ceiling: 1400 })
    expect(rungOf({ id: 10303, score: 1000 }, 'majsoul')).toEqual({ name: '雀杰三', score: 1000, ceiling: 2000 })
    // The three-player ladder counts the same way.
    expect(rungOf({ id: 20101, score: 0 }, 'majsoul')).toEqual({ name: '初心一', score: 0, ceiling: 20 })
    expect(rungOf({ id: 10403, score: 3000 }, 'majsoul').ceiling).toBe(3600)
  })

  it('names what it has no table for without making one up', () => {
    expect(rungOf({ id: 10701, score: 120 }, 'majsoul')).toEqual({ name: '魂天 Lv1', score: 120, ceiling: null })
    expect(rungOf({ id: 19999, score: 5 }, 'majsoul')).toEqual({ name: '段位 #19999', score: 5, ceiling: null })
  })

  it('is named in the language of the moment', () => {
    setLang('en')
    expect(rungOf({ id: 10303, score: 1000 }, 'majsoul').name).toBe('Expert 3')
    setLang('ja')
    expect(rungOf({ id: 10303, score: 1000 }, 'majsoul').name).toBe('雀傑3')
  })

  it('uses Tenhou native ranks and promotion thresholds without reinterpreting them as Mahjong Soul ids', () => {
    expect(rungOf({ id: 6, score: 70 }, 'tenhou')).toEqual({ name: '四级', score: 70, ceiling: 80 })
    expect(rungOf({ id: 7, score: 0 }, 'tenhou')).toEqual({ name: '三级', score: 0, ceiling: 100 })
    expect(rungOf({ id: 10, score: 200 }, 'tenhou')).toEqual({ name: '初段', score: 200, ceiling: 400 })
    expect(rungOf({ id: 11, score: 420 }, 'tenhou')).toEqual({ name: '二段', score: 420, ceiling: 800 })
    expect(rungOf({ id: 20, score: 0 }, 'tenhou')).toEqual({ name: '天凤位', score: 0, ceiling: null })
    expect(rungOf({ id: 21, score: 5 }, 'tenhou')).toEqual({ name: '段位 #21', score: 5, ceiling: null })
    setLang('en')
    expect(rungOf({ id: 6, score: 70 }, 'tenhou').name).toBe('4 Kyu')
    setLang('ja')
    expect(rungOf({ id: 6, score: 70 }, 'tenhou').name).toBe('四級')
  })
})
