import { describe, expect, it } from 'vitest'
import {
  actionLabel, alertsOf, candidatesOf, decisionMark, decisionView, describeAction, factsOf, handMarks, healthOf, meldView, newestSnapshot, ownHand,
  serverLine, decisionSound, controlLabel,
  type ForecastStatus, type MonitorDecision, type MonitorSnapshot, type Table,
} from './monitor'
import type { GameStateSnapshot, MjaiEvent, ShowItem } from '@/types'

const table: Table = { self: 0, players: 4 }
const tiles = (faces: Array<{ tile: string; side?: boolean; back?: boolean }>) => faces.map(f => `${f.side ? '_' : ''}${f.back ? '#' : f.tile}`).join(' ')

function decision(response: MjaiEvent & { meta?: unknown }, items: ShowItem[], trigger: MjaiEvent = { type: 'tsumo', actor: 0, pai: '1m' }): MonitorDecision {
  return { id: 7, number: 3, context: { game: 1, round: 1, event_seq: 10 }, started_ms: 0, completed_ms: 1, reaction_ms: 120, bot: 'demo', trigger,
    state: 'awaiting', response: { ...response, meta: { show: { items } } } as MonitorDecision['response'], error: null, confirmed_seq: null }
}
function snapshot(d: MonitorDecision, patch: Partial<MonitorSnapshot> = {}): MonitorSnapshot {
  return { control: null, forecast: { owner: 0, state: 'disabled' }, revision: 1, history_revision: 0, context: d.context, names: [], red_fives: true, game: { our_seat: 0, num_players: 4 } as GameStateSnapshot, view: null,
    round_ended: false, game_ended: false, capture: { state: 'running', descriptor: 'test' },
    bot: { state: 'ready', bot: 'demo', actor_id: 0 }, service: 'connected', service_error: null, server: { state: 'none' }, sync_error: null, sync_fault: null,
    brand: 'Demo', facts: [], autoplay_enabled: true, decision: d, previous_decision: null,
    result: null, previous_result: null, hands: [], events: [], last_event_ms: null, ...patch }
}

describe('monitor stream identities', () => {
  it('reports all forecast phases independently of formal advice, health and source', () => {
    const d = decision({ type: 'dahai', actor: 0, pai: '9m', tsumogiri: true }, [])
    const s = snapshot(d, { control: {
      context: d.context, window: null, source: 'primary',
      primary: { state: 'ready', through: 10 }, standby: { state: 'ready', through: 10 },
    } })
    const phases: Array<[ForecastStatus, string]> = [
      [{ owner: 7, state: 'idle' }, '等待对局'],
      [{ owner: 7, state: 'disabled' }, '未运行'],
      [{ owner: 7, state: 'unsupported', reason: 'protocol lacks forecast' }, '不支持'],
      [{ owner: 7, state: 'waiting' }, '等待中'],
      [{ owner: 7, state: 'running', context: d.context, through: 10, position: 6 }, '计算中'],
      [{ owner: 7, state: 'ready', context: d.context, through: 10, position: 6 }, '可用'],
      [{ owner: 7, state: 'unavailable', reason: 'forecast worker timed out' }, '不可用'],
    ]
    for (const [forecast] of phases) {
      const next = { ...s, forecast }
      expect(decisionView(next)).toEqual(decisionView(s))
      expect(decisionSound(next)).toBe(true)
      expect(healthOf(next)).toEqual(healthOf(s))
      expect(alertsOf(next, { ui: null, bridge: null })).toEqual([])
      expect(controlLabel(next.control!)).toBe('主模型 · 主模型就绪')
    }
  })

  it('keeps standby advice valid during a primary fault and still rejects a real tracker gap', () => {
    const d = decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [])
    const control: NonNullable<MonitorSnapshot['control']> = {
      context: d.context, window: { platform: 'majsoul', opened_ns: '-9223372036854775807' }, source: 'native_standby',
      primary: { state: 'recovering', through: 9, reason: 'pipe closed' }, standby: { state: 'ready', through: 10 },
    }
    const s = snapshot(d, { control, service: 'error', service_error: 'pipe closed', sync_fault: 'old primary fault' })
    expect(decisionSound(s)).toBe(true)
    expect(alertsOf(s, { ui: null, bridge: null })).toEqual([])
    expect(controlLabel(control)).toBe('本地备用 · 主模型恢复中')
    expect(controlLabel({ ...control, source: 'primary', primary: { state: 'direct_ready', delivered: 10 } })).toBe('主模型 · 主模型就绪')
    expect(controlLabel({ ...control, source: 'unavailable', primary: { state: 'direct_unavailable', delivered: 10, reason: 'pipe lost' } })).toBe('暂无有效来源 · 主模型不可用')
    expect(decisionSound({ ...s, sync_error: 'missing real event' })).toBe(false)
    expect(alertsOf({ ...s, sync_error: 'missing real event' }, { ui: null, bridge: null }).map(a => a.key)).toEqual(['sync'])
    expect(decisionSound({ ...s, control: { ...control, source: 'unavailable' } })).toBe(false)
    expect(decisionSound({ ...s, round_ended: true })).toBe(false)
  })

  it('keeps an update that arrived before the initial query response', () => {
    const live = { revision: 9 } as MonitorSnapshot
    const olderQuery = { revision: 8 } as MonitorSnapshot
    expect(newestSnapshot(live, olderQuery)).toBe(live)
    expect(newestSnapshot(null, olderQuery)).toBe(olderQuery)
  })
})

describe('actions in words and tiles', () => {
  it('preserves red tiles and riichi companion discards in the action label', () => {
    expect(actionLabel({ type: 'reach', actor: 0, pai: '5pr' })).toBe('立直 打 赤5筒')
    expect(actionLabel({ type: 'dahai', actor: 0, pai: '5p', tsumogiri: false })).toBe('打 5筒')
  })
  it('supports calls and wins instead of translating every action as a discard', () => {
    expect(actionLabel({ type: 'pon', actor: 0, target: 1, pai: 'P', consumed: ['P', 'P'] })).toBe('碰 白')
    expect(actionLabel({ type: 'hora', actor: 0, target: 0 })).toBe('自摸')
    expect(actionLabel({ type: 'hora', actor: 0, target: 2 })).toBe('荣和')
    expect(actionLabel({ type: 'none' })).toBe('过')
  })
  it('lays the claimed tile down on the side it came from', () => {
    const pon = (target: number) => tiles(describeAction({ type: 'pon', actor: 0, target, pai: '5pr', consumed: ['5p', '5p'] }, table)!.faces)
    expect(pon(3)).toBe('_5pr 5p 5p')
    expect(pon(2)).toBe('5p _5pr 5p')
    expect(pon(1)).toBe('5p 5p _5pr')
    expect(tiles(describeAction({ type: 'chi', actor: 0, target: 3, pai: '7s', consumed: ['9s', '8s'] }, table)!.faces)).toBe('_7s 8s 9s')
  })
  it('keeps a red five face up in a concealed kan', () => {
    expect(tiles(describeAction({ type: 'ankan', actor: 0, consumed: ['5m', '5m', '5mr', '5m'] }, table)!.faces)).toBe('# 5mr 5m #')
    // A riichi says how its tile leaves the hand when its bot names the copy, and says nothing when it does not.
    expect(describeAction({ type: 'reach', actor: 0, pai: '5mr', tsumogiri: false }, table)!.note).toBe('手切')
    expect(describeAction({ type: 'reach', actor: 0, pai: '5mr', tsumogiri: true }, table)!.note).toBe('摸切')
    expect(describeAction({ type: 'reach', actor: 0, pai: '5mr' }, table)!.note).toBeUndefined()
  })
  it('draws a meld from the tracker record, not from one repeated tile', () => {
    expect(tiles(meldView({ kind: 'pon', tiles: ['5sr', '5s', '5s'], from_who: 2, called_tile: '5sr' }, 3, { self: 0, players: 4 }))).toBe('_5sr 5s 5s')
    expect(tiles(meldView({ kind: 'pon', tiles: ['5s', '5sr', '5s'], from_who: 1, called_tile: '5s' }, 0, table))).toBe('5s 5sr _5s')
  })
})

describe('candidates', () => {
  it('reports probabilities as given and marks only the answered action', () => {
    const d = decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [
      { label: 'Discard', pais: ['9s'], value: '62.3%' }, { label: 'Discard', pais: ['3m'], value: '17.1%' },
      { label: 'Discard', pais: ['9s'], value: '9.4%' }, { label: 'Discard', pais: ['2s'], value: '6.2%' },
      { label: 'Discard', pais: ['5mr'], value: '3.1%' }, { label: 'Discard', pais: ['1z'], value: '1.9%' },
    ])
    const rows = candidatesOf(d, table)
    expect(rows.map(r => r.value)).toEqual(['62.3%', '17.1%', '9.4%', '6.2%', '3.1%'])
    expect(rows.map(r => Math.round(r.share! * 1000))).toEqual([623, 171, 94, 62, 31])
    // Hand discard and tsumogiri of the same tile are two rows; the first one is the answer.
    expect(rows.map(r => r.chosen)).toEqual([true, false, false, false, false])
  })
  it('tells the tsumogiri from a hand copy of the same tile when the bot says which', () => {
    const items: ShowItem[] = [
      { label: 'Discard', pais: ['9s'], value: '50%', tsumogiri: true }, { label: 'Discard', pais: ['9s'], value: '30%', tsumogiri: false },
      { label: 'Discard', pais: ['3m'], value: '20%', tsumogiri: false },
    ]
    const rows = candidatesOf(decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, items), table)
    expect(rows.map(r => r.note)).toEqual(['摸切', '', ''])
    // The bot threw the hand's 9s: the second row, though the first shows the same tile.
    expect(rows.map(r => r.chosen)).toEqual([false, true, false])
    const drawn = candidatesOf(decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: true }, items), table, { type: 'dahai', actor: 0, pai: '9s', tsumogiri: false })
    expect(drawn.map(r => [r.chosen, r.taken])).toEqual([[true, false], [false, true], [false, false]])
  })
  it('expresses riichi, calls, kans, wins and passes', () => {
    const d = decision({ type: 'chi', actor: 0, target: 3, pai: '7s', consumed: ['8s', '9s'] }, [
      { label: 'Chi', pais: ['8s', '9s', '7s'], value: '40%' }, { label: 'Pass', value: '30%' },
      { label: 'Pon', pais: ['7s', '7s', '7s'], value: '15%' }, { label: 'Kan', pais: ['7s', '7s', '7s', '7s'], value: '10%' },
      { label: 'Ron', pais: ['7s'], value: '5%' },
    ], { type: 'dahai', actor: 3, pai: '7s', tsumogiri: false })
    const rows = candidatesOf(d, table)
    expect(rows.map(r => r.verb)).toEqual(['吃', '过', '碰', '明杠', '荣和'])
    expect(tiles(rows[0].faces)).toBe('_7s 8s 9s')
    expect(rows.map(r => r.chosen)).toEqual([true, false, false, false, false])
  })
  it('marks the move the game made instead, and never on the chosen row', () => {
    const d = decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [
      { label: 'Discard', pais: ['9s'], value: '60%' }, { label: 'Discard', pais: ['5mr'], value: '30%' }, { label: 'Discard', pais: ['5m'], value: '10%' },
    ])
    const taken = (pai: string) => candidatesOf(d, table, { type: 'dahai', actor: 0, pai, tsumogiri: false }).map(r => r.taken)
    // A red five and a plain five are different moves.
    expect(taken('5m')).toEqual([false, false, true])
    expect(taken('5mr')).toEqual([false, true, false])
    expect(taken('1z')).toEqual([false, false, false])
    expect(candidatesOf(d, table).some(r => r.taken)).toBe(false)
  })
  it('tells an own-turn kan from an open one and riichi from its discard', () => {
    const d = decision({ type: 'reach', actor: 0, pai: '3p' }, [
      { label: 'Riichi', value: '70%' }, { label: 'Discard', pais: ['3p'], value: '20%' },
      { label: 'Kan', pais: ['9p', '9p', '9p', '9p'], value: '6%' }, { label: 'Kan', pais: ['C', 'C', 'C'], value: '3%' },
      { label: 'Tsumo', pais: ['1m'], value: '1%' },
    ])
    const rows = candidatesOf(d, table)
    expect(rows.map(r => r.verb)).toEqual(['立直', '打', '暗杠', '加杠', '自摸'])
    expect(rows.map(r => r.chosen)).toEqual([true, false, false, false, false])
    // A riichi row shows the tile its declaration throws when the bot names it, and its copy.
    const named = candidatesOf(decision({ type: 'reach', actor: 0, pai: '3p', tsumogiri: true }, [
      { label: 'Riichi', pais: ['3p'], value: '70%', tsumogiri: true }, { label: 'Discard', pais: ['3p'], value: '20%', tsumogiri: true },
    ]), table)
    expect(tiles(named[0].faces)).toBe('3p')
    expect(named[0].note).toBe('摸切')
    expect(rows[0].faces).toEqual([])
  })
})

describe('where a decision stands', () => {
  const discard = () => decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [{ label: 'Discard', pais: ['9s'], value: '60%' }])
  it('goes by the position its bot gives it, and otherwise by its number in the game', () => {
    const placed = (position: unknown): MonitorDecision => {
      const d = discard()
      return { ...d, response: { ...d.response!, meta: { show: { items: [], position } } } as MonitorDecision['response'] }
    }
    expect(decisionMark(discard())).toBe('#3')
    expect(decisionMark(placed(' step 187 '))).toBe('step 187')
    // Not the process-wide id, and nothing that is not a short string.
    expect(decisionMark({ ...discard(), id: 412 })).toBe('#3')
    expect(decisionMark(placed(187))).toBe('#3')
    expect(decisionMark(placed(''))).toBe('#3')
    expect(decisionMark(placed('the hundred and eighty-seventh step'))).toBe('#3')
    // No reply, no answer to which it is: a failed request falls back, a pending one waits.
    expect(decisionMark({ ...discard(), state: 'failed', response: null })).toBe('#3')
    expect(decisionMark({ ...placed('step 187'), state: 'thinking', response: null })).toBe('')
  })
  it('is live only between the suggestion and the game answering it', () => {
    expect(decisionView(snapshot(discard()))).toMatchObject({ phase: 'awaiting', live: true, steps: ['done', 'done', 'active'] })
    expect(decisionView(snapshot({ ...discard(), state: 'thinking', response: null }))).toMatchObject({ phase: 'thinking', live: false, action: null })
    expect(decisionView(snapshot({ ...discard(), state: 'confirmed', confirmed_seq: 11 }))).toMatchObject({ phase: 'confirmed', tone: 'done', live: false, steps: ['done', 'done', 'done'] })
  })
  it('stops being live on a fault or at the end of the round', () => {
    expect(decisionView(snapshot(discard(), { service: 'error' })).live).toBe(false)
    expect(decisionView(snapshot(discard(), { sync_error: 'gap' })).live).toBe(false)
    expect(decisionView(snapshot(discard(), { round_ended: true })).live).toBe(false)
  })
  it('separates a contradicted suggestion from one that merely lapsed', () => {
    const events = [{ seq: 11, ts_ms: 0, event: { type: 'dahai', actor: 0, pai: '3m', tsumogiri: false } as MjaiEvent }]
    const contradicted = decisionView(snapshot({ ...discard(), state: 'expired', error: 'The game confirmed a different action.' }, { events }))
    expect(contradicted).toMatchObject({ phase: 'mismatch', steps: ['done', 'done', 'bad'] })
    expect(contradicted.actual?.object).toBe('3万')
    const lapsed = decisionView(snapshot({ ...discard(), state: 'expired' }, { round_ended: true }))
    expect(lapsed).toMatchObject({ phase: 'expired', actual: null })
  })
  it('treats a pass as a suggestion with nothing to confirm', () => {
    const pass = { ...decision({ type: 'none' }, [{ label: 'Pass', value: '80%' }]), state: 'declined' as const }
    expect(decisionView(snapshot(pass))).toMatchObject({ phase: 'declined', live: true, steps: ['done', 'done', 'void'] })
  })
})

describe('links', () => {
  it('reports capture, Bot process, model service and sync as four separate things', () => {
    const d = decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [])
    // A ready Bot process with the model service down and the table out of sync.
    const health = healthOf(snapshot(d, { service: 'error', service_error: 'Socket closed', sync_error: 'gap' }))
    expect(health.map(h => [h.key, h.tone])).toEqual([['capture', 'ok'], ['bot', 'ok'], ['service', 'bad'], ['sync', 'bad']])
    expect(healthOf(null).map(h => h.tone)).toEqual(['idle', 'idle', 'idle', 'idle'])
  })

  it('puts the server the app runs before the connection the bot reports', () => {
    const d = decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [])
    const service = (patch: Partial<MonitorSnapshot>) => healthOf(snapshot(d, patch))[2]
    // Up, with no game yet to have reached it through: ready is not connected.
    expect(service({ service: 'unknown', server: { state: 'ready', bot: 'demo', label: 'Model A', facts: [], since_ms: 0, startup_ms: 19_000 } }))
      .toMatchObject({ tone: 'ok', state: '就绪' })
    expect(service({ server: { state: 'ready', bot: 'demo', label: null, facts: [], since_ms: 0, startup_ms: 1 } })).toMatchObject({ state: '已连接' })
    expect(service({ service: 'unknown', server: { state: 'starting', bot: 'demo', since_ms: 0 } })).toMatchObject({ tone: 'busy', state: '加载中' })
    expect(service({ service: 'unknown', server: { state: 'installing', bot: 'demo' } })).toMatchObject({ tone: 'busy', state: '安装中' })
    // A server that is down outranks whatever the bot last said about it.
    expect(service({ service: 'connected', server: { state: 'failed', bot: 'demo', error: 'it exited (exit code: 1)', retry_at_ms: 4_000 } }))
      .toMatchObject({ tone: 'bad', state: '未运行', detail: 'it exited (exit code: 1)' })
  })

  it('raises one alert for a server that is down, not a second for the lost connection', () => {
    const d = decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [])
    const down = snapshot(d, { service: 'error', service_error: 'refused', server: { state: 'failed', bot: 'demo', error: 'it exited', retry_at_ms: 4_000 } })
    expect(alertsOf(down, { ui: null, bridge: null }).map(a => a.key)).toEqual(['server'])
    expect(alertsOf(snapshot(d, { service: 'error', service_error: 'refused' }), { ui: null, bridge: null }).map(a => a.key)).toEqual(['service'])
  })

  it('says what the server is doing, under the name it gave itself', () => {
    const d = decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [])
    const line = (server: MonitorSnapshot['server']) => serverLine(snapshot(d, { server }), 10_000)
    expect(line({ state: 'starting', bot: 'demo', since_ms: 4_000 })).toBe('已启动 6 秒，正在加载')
    expect(line({ state: 'failed', bot: 'demo', error: 'x', retry_at_ms: 12_000 })).toBe('2 秒后重新启动')
    expect(line({ state: 'ready', bot: 'demo', label: 'Model A', facts: [], since_ms: 0, startup_ms: 19_400 })).toBe('Model A · 应用托管 · 启动 19 秒')
    // A service that names nothing goes by its bot's name.
    expect(line({ state: 'ready', bot: 'demo', label: null, facts: [], since_ms: 0, startup_ms: 600 })).toBe('demo · 应用托管 · 启动 1 秒')
    expect(line({ state: 'none' })).toBe('收到第一份候选后确认，与 Bot 进程状态无关')
    expect(serverLine(null, 0)).toBe('收到第一份候选后确认，与 Bot 进程状态无关')
  })
})

describe('what a bot says about itself', () => {
  const d = decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, [])
  const depth = { name: 'Depth', value: '42 plies', note: 'Counted from the deal.' }
  const device = { name: 'Device', value: 'CPU' }

  it('is shown as written, the latest reply before the service', () => {
    const s = snapshot(d, { facts: [depth], server: { state: 'ready', bot: 'demo', label: 'Model A', facts: [device], since_ms: 0, startup_ms: 1 } })
    expect(factsOf(s)).toEqual([depth, device])
    // A service that is not up has nothing current to say.
    expect(factsOf(snapshot(d, { facts: [depth], server: { state: 'starting', bot: 'demo', since_ms: 0 } }))).toEqual([depth])
    expect(factsOf(null)).toEqual([])
  })

  it('keeps to the room there is', () => {
    const many = Array.from({ length: 5 }, (_, i) => ({ name: `Fact ${i}`, value: String(i) }))
    expect(factsOf(snapshot(d, { facts: many })).map(f => f.name)).toEqual(['Fact 0', 'Fact 1', 'Fact 2'])
  })

  it('names the values of its own candidates', () => {
    const rows = [{ label: 'Discard', pais: ['9s'], value: '60.0%' }]
    const reply = { ...decision({ type: 'dahai', actor: 0, pai: '9s', tsumogiri: false }, rows) }
    // The rows are read the same whatever the bot calls their values.
    expect(candidatesOf(reply, table)[0]).toMatchObject({ verb: '打', share: 0.6, value: '60.0%', chosen: true })
  })

  it('reads the labels of both card vocabularies', () => {
    const verbs = (label: string, pais: string[], trigger: MjaiEvent = { type: 'tsumo', actor: 0, pai: '1m' }) =>
      candidatesOf(decision({ type: 'none' }, [{ label, pais, value: '1%' }], trigger), table)[0].verb
    expect(['Ankan', 'Kakan', 'Hora', 'Ryukyoku', 'Kita'].map(label => verbs(label, label === 'Ankan' ? ['1m', '1m', '1m', '1m'] : ['N'])))
      .toEqual(['暗杠', '加杠', '和牌', '九种九牌', '拔北'])
    expect(['Ron', 'Tsumo', 'Nine terminals', 'Pass'].map(label => verbs(label, []))).toEqual(['荣和', '自摸', '九种九牌', '过'])
  })
})

describe('own hand', () => {
  const game = (tehai: string[], drawn: string | null) => ({ our_seat: 0, num_players: 4, players: [{ seat: 0, tehai, drawn_tile: drawn, melds: [] }] }) as unknown as GameStateSnapshot
  it('stands the draw apart and keeps a red five where it sorts', () => {
    expect(ownHand(game(['9s', '5mr', '2s', '5m', '2s', 'E'], '2s'))).toEqual({ tiles: ['5m', '5mr', '2s', '9s', 'E'], drawn: '2s' })
  })
  it('lifts the tile a discard takes, preferring the draw only for tsumogiri', () => {
    const hand = { tiles: ['5m', '5mr', '2s', '9s'], drawn: '2s' }
    expect(handMarks(hand, { type: 'dahai', actor: 0, pai: '2s', tsumogiri: true })).toEqual({ tiles: new Set(), drawn: true })
    expect(handMarks(hand, { type: 'dahai', actor: 0, pai: '2s', tsumogiri: false })).toEqual({ tiles: new Set([2]), drawn: false })
    // 5m and 5mr are different tiles to discard.
    expect(handMarks(hand, { type: 'reach', actor: 0, pai: '5mr' })).toEqual({ tiles: new Set([1]), drawn: false })
    expect(handMarks(hand, { type: 'chi', actor: 0, target: 3, pai: '1s', consumed: ['2s', '2s'] } as MjaiEvent)).toEqual({ tiles: new Set([2]), drawn: true })
  })
})
