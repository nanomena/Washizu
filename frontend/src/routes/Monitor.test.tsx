import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import type { MonitorEvent, MonitorSnapshot } from '@/lib/monitor'
import { setLang } from '@/lib/i18n'

const fixture = vi.hoisted(() => ({
  snapshot: null as MonitorSnapshot | null,
  options: { always_on_top: false } as { always_on_top: boolean; pointer_trail?: boolean; pointer_lean?: boolean; quick_confirm?: boolean; riichi_auto_win?: boolean; auto_continue?: boolean; stream_output?: boolean; stream_url?: string },
  calls: [] as Array<[string, unknown]>,
  /** What the core answers when an earlier hand's events are asked for. */
  handEvents: [] as MonitorEvent[],
  /** The games on record. */
  history: [] as unknown[],
}))
vi.mock('@/hooks/useMonitor', () => ({ useMonitor: () => ({ snapshot: fixture.snapshot, error: null }) }))
vi.mock('@/lib/host', () => ({
  listen: vi.fn(() => () => {}),
  invoke: vi.fn(async (cmd: string, args?: unknown) => {
    fixture.calls.push([cmd, args])
    if (cmd === 'set_monitor_pinned') fixture.options = { ...fixture.options, always_on_top: (args as { pinned: boolean }).pinned }
    if (cmd === 'set_monitor_trail') fixture.options = { ...fixture.options, pointer_trail: (args as { enabled: boolean }).enabled }
    if (cmd === 'set_monitor_lean') fixture.options = { ...fixture.options, pointer_lean: (args as { enabled: boolean }).enabled }
    if (cmd === 'set_monitor_confirm') fixture.options = { ...fixture.options, quick_confirm: (args as { enabled: boolean }).enabled }
    if (cmd === 'set_monitor_auto_win') fixture.options = { ...fixture.options, riichi_auto_win: (args as { enabled: boolean }).enabled }
    if (cmd === 'set_monitor_continue') fixture.options = { ...fixture.options, auto_continue: (args as { enabled: boolean }).enabled }
    if (cmd === 'set_monitor_stream') {
      const enabled = (args as { enabled: boolean }).enabled
      fixture.options = { ...fixture.options, stream_output: enabled, stream_url: enabled ? 'http://127.0.0.1:50123/0123456789abcdef01234567/' : undefined }
    }
    if (cmd === 'get_hand_events') return fixture.handEvents
    if (cmd === 'get_history') return fixture.history
    return ['get_monitor_options', 'set_monitor_pinned', 'set_monitor_trail', 'set_monitor_lean', 'set_monitor_confirm', 'set_monitor_auto_win', 'set_monitor_continue', 'set_monitor_stream'].includes(cmd) ? fixture.options : undefined
  }),
}))
// No canvas under jsdom, so mahgen cannot draw. A tile is then an <img> whose
// alt text names it, which is what the assertions read.
vi.mock('@/lib/tileFace', () => ({ faceSeq: (tile: string) => tile, useFace: () => 'data:,' }))
import { Monitor } from './Monitor'

function state(): MonitorSnapshot {
  const context = { game: 1, round: 1, event_seq: 10 }
  return {
    revision: 1, context, names: [], red_fives: true,
    game: null, view: null, round_ended: false, game_ended: false,
    capture: { state: 'running', descriptor: 'test' },
    bot: { state: 'ready', bot: 'demo', actor_id: 0 }, service: 'connected', service_error: null, server: { state: 'none' }, sync_error: null,
    brand: 'Demo', facts: [], autoplay_enabled: true,
    previous_decision: null, result: null, previous_result: null, hands: [], events: [], last_event_ms: null,
    decision: { id: 41, number: 1, context, started_ms: Date.now(), completed_ms: Date.now(),
      reaction_ms: 120, bot: 'demo', trigger: { type: 'tsumo', actor: 0, pai: '9m' }, state: 'awaiting', error: null, confirmed_seq: null,
      response: { type: 'dahai', actor: 0, pai: '9m', tsumogiri: true, meta: { show: { items: [
        { label: 'Discard', pais: ['9m'], value: '60.0%' }, { label: 'Discard', pais: ['5pr'], value: '25.0%' },
      ] } } } },
  }
}
const decision = () => screen.getByRole('status').closest('section')!
const candidates = () => screen.queryAllByRole('listitem').filter(li => li.className.includes('mcand'))

describe('monitor user states', () => {
  beforeEach(() => { fixture.snapshot = state(); fixture.options = { always_on_top: false }; fixture.calls = []; fixture.handEvents = [] })

  it('shows a suggestion the game has not answered yet as pending, never as done', () => {
    render(<Monitor />)
    expect(screen.getByRole('status').textContent).toBe('待确认')
    expect(within(decision()).getByText('打')).toBeTruthy()
    expect(within(decision()).getByText('9万')).toBeTruthy()
    expect(candidates()).toHaveLength(2)
    // Values as reported: two rows of five are not rescaled to 100%.
    expect(candidates().map(li => li.textContent)).toEqual(['1打模型选择60.0%', '2打25.0%'])
    expect(within(candidates()[1]).getByRole('img', { name: '赤5筒' })).toBeTruthy()
  })

  it('numbers a decision by its place in the game, or by the place its bot gives it', () => {
    const { unmount } = render(<Monitor />)
    // Its number in the game, not the id that runs for the life of the process (41).
    expect(within(decision()).getByText('#1')).toBeTruthy()
    unmount()
    const show = fixture.snapshot!.decision!.response!.meta!.show as Record<string, unknown>
    show.position = 'step 187'
    render(<Monitor />)
    expect(within(decision()).getByText('step 187')).toBeTruthy()
    expect(within(decision()).queryByText('#1')).toBeNull()
  })

  it('does not mistake a ready Bot process for a connected model service', () => {
    fixture.snapshot!.service = 'error'; fixture.snapshot!.service_error = 'Socket closed'
    fixture.snapshot!.decision!.state = 'expired'
    render(<Monitor />)
    // The links have one light, on the settings' tab, and it says which link is down.
    const links = screen.getByRole('tab', { name: '设置' })
    expect(links.querySelector('.mdot')?.getAttribute('data-tone')).toBe('bad')
    expect(links.title).toBe('连接：模型断开')
    expect(screen.getByRole('alert').textContent).toContain('模型服务断开')
    // The action stays as history; its candidates do not stay as current ones.
    expect(screen.getByRole('status').textContent).toBe('已失效')
    expect(within(decision()).getByText('9万')).toBeTruthy()
    expect(candidates()).toHaveLength(0)
  })

  it('shows a tab while the pointer rests on it, and keeps the one clicked', async () => {
    const { container } = render(<Monitor />)
    const shown = () => container.querySelector('[role="tabpanel"]')!.getAttribute('data-tab')
    fireEvent.mouseEnter(screen.getByRole('tab', { name: '本局' }))
    await waitFor(() => expect(shown()).toBe('hand'))
    // Leaving the dock goes back to the tab kept.
    fireEvent.mouseLeave(container.querySelector('aside.md')!)
    expect(shown()).toBe('decision')
    fireEvent.mouseEnter(screen.getByRole('tab', { name: '对局' }))
    fireEvent.click(screen.getByRole('tab', { name: '对局' }))
    fireEvent.mouseLeave(container.querySelector('aside.md')!)
    expect(shown()).toBe('game')
  })

  it('keeps the confirmed action as history but drops its candidates at round end', () => {
    fixture.snapshot!.decision!.state = 'confirmed'
    fixture.snapshot!.round_ended = true
    render(<Monitor />)
    expect(screen.getByRole('status').textContent).toBe('已确认')
    expect(screen.getByText('上一决策')).toBeTruthy()
    expect(candidates()).toHaveLength(0)
    expect(screen.getByText('本局已结束，候选不再有效')).toBeTruthy()
  })

  it('names the move the game made instead when it contradicts the suggestion', () => {
    const s = fixture.snapshot!
    s.events = [{ seq: 11, ts_ms: Date.now(), event: { type: 'dahai', actor: 0, pai: '1s', tsumogiri: false } }]
    s.decision = { ...s.decision!, state: 'expired', error: 'The game confirmed a different action.' }
    render(<Monitor />)
    expect(screen.getByRole('status').textContent).toBe('不一致')
    const actual = screen.getByText('雀魂实际').parentElement!
    expect(within(actual).getByText('1索')).toBeTruthy()
    // What the model weighed stays as a record, marked void — not as a live suggestion.
    expect(candidates()).toHaveLength(2)
    expect(candidates()[0].closest('section')!.getAttribute('data-state')).toBe('void')
    expect(screen.getByText(/已失效/)).toBeTruthy()
  })

  it('points at the candidate the game took when it is one of those listed', () => {
    const s = fixture.snapshot!
    s.events = [{ seq: 11, ts_ms: Date.now(), event: { type: 'dahai', actor: 0, pai: '5pr', tsumogiri: false } }]
    s.decision = { ...s.decision!, state: 'expired', error: 'The game confirmed a different action.' }
    render(<Monitor />)
    expect(candidates().map(li => li.textContent)).toEqual(['1打模型选择60.0%', '2打雀魂实际25.0%'])
  })

  it('keeps the decision in place on every tab of the dock', () => {
    render(<Monitor />)
    for (const name of ['本局', '对局', '统计', '设置']) {
      fireEvent.click(screen.getByRole('tab', { name }))
      expect(screen.getByRole('status').textContent).toBe('待确认')
      expect(within(decision()).getByText('9万')).toBeTruthy()
    }
    fireEvent.click(screen.getByRole('button', { name: '连接' }))
    expect(screen.getByText('牌局同步')).toBeTruthy()
    expect(screen.getByRole('status').textContent).toBe('待确认')
  })

  it('shows no action and no candidates while the model is still being asked', () => {
    fixture.snapshot!.decision = { ...fixture.snapshot!.decision!, state: 'thinking', response: null, reaction_ms: null, completed_ms: null }
    render(<Monitor />)
    expect(screen.getByRole('status').textContent).toBe('推理中')
    expect(within(decision()).queryByText('9万')).toBeNull()
    expect(candidates()).toHaveLength(0)
  })

  it('offers autoplay in three levels that say what each does, and sets them through the backend', async () => {
    const { unmount } = render(<Monitor />)
    const levels = screen.getByRole('radiogroup', { name: '自动操作' })
    const level = (name: string) => within(levels).getByRole('radio', { name })
    expect(level('打牌').getAttribute('aria-checked')).toBe('true')
    expect(level('连战').getAttribute('title')).toMatch(/再来一场/)
    await waitFor(() => expect((level('关') as HTMLButtonElement).disabled).toBe(false))
    fireEvent.click(level('关'))
    await waitFor(() => expect(fixture.calls).toContainEqual(['set_monitor_autoplay', { enabled: false }]))
    // Off leaves playing on as it was; the level shows what the backend reports, not what was just clicked.
    expect(fixture.calls.some(([cmd]) => cmd === 'set_monitor_continue')).toBe(false)
    expect(level('打牌').getAttribute('aria-checked')).toBe('true')
    // Game after game: playing on first, then autoplay.
    fireEvent.click(level('连战'))
    await waitFor(() => expect(fixture.calls.slice(-2)).toEqual([['set_monitor_continue', { enabled: true }], ['set_monitor_autoplay', { enabled: true }]]))
    await waitFor(() => expect(level('连战').getAttribute('aria-checked')).toBe('true'))
    unmount()
    // With autoplay off, the level is off whatever playing on says.
    fixture.snapshot!.autoplay_enabled = false
    render(<Monitor />)
    await waitFor(() => expect(within(screen.getByRole('radiogroup', { name: '自动操作' })).getByRole('radio', { name: '关' }).getAttribute('aria-checked')).toBe('true'))
  })

  it('opens the game in the window by itself, once, when nothing is being played', async () => {
    const stage = { x: 0, y: 0, width: 960, height: 540 }
    const rect = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({ ...stage, top: 0, left: 0, right: 960, bottom: 540, toJSON: () => stage } as DOMRect)
    fixture.snapshot = { ...state(), decision: null, capture: { state: 'stopped' }, game_ended: true, round_ended: true }
    const { rerender } = render(<Monitor />)
    await waitFor(() => expect(fixture.calls.some(([cmd]) => cmd === 'open_monitor_game')).toBe(true))
    rerender(<Monitor />)
    expect(fixture.calls.filter(([cmd]) => cmd === 'open_monitor_game')).toEqual([['open_monitor_game', { bounds: { ...stage, visible: true } }]])
    rect.mockRestore()
  })

  it('does not open the game again when capture says its view is already there', async () => {
    render(<Monitor />)
    await waitFor(() => expect((screen.getByRole('button', { name: '置顶' }) as HTMLButtonElement).disabled).toBe(false))
    expect(fixture.calls.some(([cmd]) => cmd === 'open_monitor_game')).toBe(false)
  })

  it('shows a partial draw reveal without filling in the seats that stayed hidden', () => {
    fixture.snapshot!.round_ended = true
    fixture.snapshot!.result = { kind: 'exhaustive', scores: null, deltas: null,
      hands: [{ seat: 1, tiles: ['5pr', 'P'], winning_tile: null, target: null, ura_markers: [] }] }
    render(<Monitor />)
    expect(screen.getByText('荒牌流局')).toBeTruthy()
    expect(screen.getByText('座位 2')).toBeTruthy()
    expect(screen.getAllByText('公开手牌')).toHaveLength(1)
    expect(screen.queryByText('座位 3')).toBeNull()
  })

  it('shows a winning tile once, apart from the hand it completed', () => {
    fixture.snapshot!.round_ended = true
    fixture.snapshot!.result = { kind: 'hora', scores: [24000, 26000], deltas: [-1000, 1000],
      hands: [{ seat: 1, tiles: ['1m', '1m'], winning_tile: 'C', target: 1, ura_markers: [] }] }
    render(<Monitor />)
    const reveal = screen.getByText('自摸').closest('.mreveal') as HTMLElement
    expect(within(reveal).getAllByRole('img').map(img => img.getAttribute('alt'))).toEqual(['1万', '1万', '中'])
    expect(screen.getByText('+1,000')).toBeTruthy()
  })

  it('shows an earlier hand with the melds it ended with, once the next round is dealt', () => {
    fixture.snapshot!.result = null
    fixture.snapshot!.hands = [
      { round: 1, bakaze: 'E', kyoku: 1, honba: 0, events: 50, result: { kind: 'hora', scores: [24000, 26000], deltas: [-1000, 1000],
        hands: [{ seat: 1, tiles: ['1m', '1m'], melds: [{ kind: 'pon', tiles: ['P', 'P', 'P'], from_who: 0, called_tile: 'P' }], winning_tile: 'C', target: 1, ura_markers: [] }] } },
      { round: 2, bakaze: 'E', kyoku: 2, honba: 0, result: null, events: 3 },
    ]
    render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '对局' }))
    fireEvent.click(screen.getByRole('button', { name: /东1局/ }))
    const reveal = screen.getByText('自摸').closest('.mreveal') as HTMLElement
    const shown = within(reveal).getAllByRole('img').map(img => img.getAttribute('alt'))
    expect(shown.slice(0, 3)).toEqual(['1万', '1万', '中'])
    expect(shown.filter(alt => alt === '白')).toHaveLength(3)
  })

  it('is one window with one control of its own: staying on top', async () => {
    render(<Monitor />)
    const pin = screen.getByRole('button', { name: '置顶' })
    await waitFor(() => expect((pin as HTMLButtonElement).disabled).toBe(false))
    expect(pin.getAttribute('aria-pressed')).toBe('false')
    fireEvent.click(pin)
    await waitFor(() => expect(pin.getAttribute('aria-pressed')).toBe('true'))
    expect(fixture.calls).toContainEqual(['set_monitor_pinned', { pinned: true }])
    // No second shape to fold into, and no other window to go to.
    expect(screen.queryByRole('button', { name: '收起' })).toBeNull()
    expect(screen.queryByRole('button', { name: '主界面' })).toBeNull()
  })

  it('puts the game exactly where the stage is once it is open in this window', async () => {
    const stage = { x: 0, y: 0, width: 960, height: 540 }
    const rect = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      return (this.className === 'mon-stage' ? { ...stage, top: 0, left: 0, right: 960, bottom: 540, toJSON: () => stage } : { x: 0, y: 0, width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0, toJSON: () => ({}) }) as DOMRect
    })
    render(<Monitor />)
    await waitFor(() => expect(fixture.calls.some(([cmd]) => cmd === 'resize_monitor_game')).toBe(true))
    expect(fixture.calls.filter(([cmd]) => cmd === 'resize_monitor_game').at(-1)![1]).toEqual({ bounds: { ...stage, visible: true } })
    rect.mockRestore()
  })
})

describe('the game as a whole', () => {
  const draw = { kind: 'draw', hands: [], scores: [25000, 25000, 25000, 25000], deltas: [0, 0, 0, 0] }
  const win = { kind: 'hora', hands: [], scores: [33000, 17000, 25000, 25000], deltas: [8000, -8000, 0, 0] }
  beforeEach(() => { fixture.snapshot = state(); fixture.options = { always_on_top: false }; fixture.calls = []; fixture.handEvents = [] })

  it('shows the links as one light that is green when everything the window runs is up', () => {
    const { unmount } = render(<Monitor />)
    const light = () => screen.getByRole('tab', { name: '设置' }).querySelector('.mdot')?.getAttribute('data-tone')
    expect(light()).toBe('ok')
    expect(screen.queryByRole('button', { name: '连接状态' })).toBeNull()
    unmount()
    // A Bot process with no game to play is not a fault; a game view that is not open is not "up".
    fixture.snapshot!.bot = { state: 'idle' }
    render(<Monitor />)
    expect(light()).toBe('ok')
    cleanup()
    fixture.snapshot!.capture = { state: 'stopped' }
    render(<Monitor />)
    expect(light()).toBe('idle')
  })

  it('lists every hand of the game, and shows the one picked: how it ended, or how it went', async () => {
    fixture.snapshot!.hands = [
      { round: 1, bakaze: 'E', kyoku: 1, honba: 0, result: draw, events: 40 },
      { round: 2, bakaze: 'E', kyoku: 1, honba: 1, result: win, events: 55 },
      { round: 3, bakaze: 'E', kyoku: 2, honba: 0, result: null, events: 3 },
    ]
    fixture.handEvents = [{ seq: 12, ts_ms: 0, event: { type: 'dahai', actor: 1, pai: '3m', tsumogiri: false } }]
    render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '对局' }))
    const detail = () => document.querySelector('.mgame-detail') as HTMLElement
    const line = (name: RegExp) => screen.getByRole('button', { name })
    // A line a hand, by how it ended; the hand in play has not.
    expect(line(/东1局.*流局/)).toBeTruthy()
    expect(line(/东1局.*1 本场.*和牌/)).toBeTruthy()
    expect(line(/东2局/).textContent).toContain('进行中')
    // The hand in play is the one shown, and it has only its events.
    expect(line(/东2局/).getAttribute('aria-pressed')).toBe('true')
    expect(within(detail()).getByText('东2局 0 本场')).toBeTruthy()
    expect((within(detail()).getByRole('button', { name: '结局' }) as HTMLButtonElement).disabled).toBe(true)

    // A finished hand opens on its end.
    fireEvent.click(line(/东1局.*1 本场/))
    expect(line(/东1局.*1 本场/).getAttribute('aria-pressed')).toBe('true')
    expect(within(detail()).getByText('东1局 1 本场')).toBeTruthy()
    expect(within(detail()).getByText('和牌')).toBeTruthy()
    expect(within(detail()).getByText('+8,000')).toBeTruthy()
    // How it went is asked for once.
    fireEvent.click(within(detail()).getByRole('button', { name: '过程' }))
    expect(await within(detail()).findByText('手切')).toBeTruthy()
    expect(fixture.calls.filter(([cmd]) => cmd === 'get_hand_events')).toEqual([['get_hand_events', { round: 2 }]])
  })

  it('follows the hand in play as it goes, and keeps a hand picked when the next is dealt', () => {
    fixture.snapshot!.hands = [
      { round: 1, bakaze: 'E', kyoku: 1, honba: 0, result: draw, events: 40 },
      { round: 2, bakaze: 'E', kyoku: 1, honba: 1, result: null, events: 1 },
    ]
    fixture.snapshot!.events = [{ seq: 90, ts_ms: 0, event: { type: 'reach', actor: 2 } }]
    const { rerender } = render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '对局' }))
    expect(screen.getByText('宣告立直')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: /东1局.*流局/ }))
    expect(screen.getByText('东1局 0 本场')).toBeTruthy()
    fixture.snapshot = { ...fixture.snapshot!, revision: 2, hands: [...fixture.snapshot!.hands, { round: 3, bakaze: 'E', kyoku: 2, honba: 0, result: null, events: 0 }] }
    rerender(<Monitor />)
    expect(screen.getByRole('button', { name: /东1局.*流局/ }).getAttribute('aria-pressed')).toBe('true')
  })
})

describe('the record', () => {
  const game = (rank: number, points: number | null, room: number, stats: Record<string, number>, yaku: Record<string, number> = {}) => ({
    id: `G${rank}${room}`, ended_at: '2026-01-01T00:00:00Z', platform: 'majsoul', num_players: 4, our_seat: 0,
    final_scores: [25000, 25000, 25000, 25000], our_rank: rank, stats, yaku,
    match_info: { platform: 'majsoul', mode_id: room }, ...(points == null ? {} : { our_rank_points: points }),
  })
  beforeEach(() => {
    fixture.snapshot = state(); fixture.options = { always_on_top: false }; fixture.calls = []; fixture.handEvents = []
    fixture.history = [
      // A friends' table: no rank points to speak of, and a record from before yaku were kept.
      game(2, 0, 0, { round: 8, agari: 2, houjuu: 0, riichi: 1 }),
      game(1, 120, 9, { round: 10, agari: 3, houjuu: 1, riichi: 2 }, { 2: 2, 12: 1 }),
      game(4, -165, 9, { round: 10, agari: 1, houjuu: 3, riichi: 1 }, { 2: 1 }),
    ]
  })

  it('adds up rank points, placement, the usual figures and yaku, for the room we play in or all', async () => {
    const { unmount } = render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '统计' }))
    // The latest game was in the gold room: its two games, 120 - 165.
    expect(await screen.findByText('2 场的段位分变化')).toBeTruthy()
    const scope = () => screen.getByRole('combobox', { name: '统计范围' }) as HTMLSelectElement
    expect(scope().value).toBe('gold')
    expect(screen.getByText('−45')).toBeTruthy()
    const figure = (name: string) => screen.getByText(name).closest('.mfact')!.querySelector('dd')!.textContent
    expect(figure('和了率')).toBe('20.0%')
    expect(screen.getByText('平均顺位').textContent).toBe('平均顺位 2.50')

    fireEvent.change(scope(), { target: { value: 'all' } })
    expect(screen.getByText('3 场的段位分变化')).toBeTruthy()
    expect(figure('和了率')).toBe('21.4%')
    expect(figure('放铳率')).toBe('14.3%')
    // Yaku count against the wins of the games that record them: 3 of 4.
    const yaku = screen.getByText('立直').closest('li')!
    expect(yaku.textContent).toBe('立直375.0%')
    expect(screen.getByText('平均顺位').textContent).toBe('平均顺位 2.33')

    // Opened again, it is on the room we play in again.
    unmount()
    render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '统计' }))
    expect(await screen.findByText('2 场的段位分变化')).toBeTruthy()
    expect(scope().value).toBe('gold')
  })

  it('keeps its three panels in place with nothing on record', async () => {
    fixture.history = []
    render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '统计' }))
    expect(await screen.findByText('还没有记录到段位分')).toBeTruthy()
    expect(screen.getByText('役种从记录它的版本起开始累计')).toBeTruthy()
    expect(screen.getByText('和了率').closest('.mfact')!.querySelector('dd')!.textContent).toBe('—')
  })
})

describe('the settings', () => {
  beforeEach(() => {
    fixture.snapshot = state(); fixture.options = { always_on_top: false }; fixture.calls = []; fixture.handEvents = []; fixture.history = []
    localStorage.removeItem('akagi.settings.group')
  })
  const group = (name: string) => fireEvent.click(screen.getByRole('button', { name }))
  const line = (name: string) => screen.getByText(name).closest('.mfact')!.querySelector('dd')!.textContent

  it('says what it runs, a group at a time', async () => {
    fixture.snapshot!.server = { state: 'ready', bot: 'demo', label: 'Model A', facts: [{ name: 'Device', value: 'CPU' }], since_ms: 0, startup_ms: 900 }
    Object.assign(fixture.options, { version: '0.1.0' })
    render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '设置' }))
    // What autoplay does is the group that opens first.
    expect(screen.getByRole('button', { name: '自动操作' }).getAttribute('aria-pressed')).toBe('true')
    expect(screen.queryByText('Model A')).toBeNull()
    group('模型与服务')
    expect(line('模型')).toBe('Model A')
    expect(line('Device')).toBe('CPU')
    expect(screen.queryByRole('switch', { name: '快速确认' })).toBeNull()
    group('应用')
    await waitFor(() => expect(line('版本')).toBe('0.1.0'))
  })

  it('holds the links, a line each, and opens on them from a fault', () => {
    fixture.snapshot!.service = 'error'; fixture.snapshot!.service_error = 'Socket closed'
    fixture.snapshot!.decision!.state = 'expired'
    render(<Monitor />)
    fireEvent.click(screen.getByRole('button', { name: /连接详情/ }))
    expect(screen.getByRole('tab', { name: '设置' }).getAttribute('aria-selected')).toBe('true')
    const links = screen.getByRole('button', { name: '连接' })
    expect(links.getAttribute('aria-pressed')).toBe('true')
    expect(links.querySelector('.mdot')?.getAttribute('data-tone')).toBe('bad')
    const service = screen.getByText('模型服务').closest('.mset-link')!
    expect(service.getAttribute('data-tone')).toBe('bad')
    expect(service.textContent).toContain('断开')
    expect(service.textContent).toContain('Socket closed')
    expect(screen.getByText('采集').closest('.mset-link')!.getAttribute('data-tone')).toBe('ok')
  })

  it('opens the group last looked at again', () => {
    const { unmount } = render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '设置' }))
    group('画面与窗口')
    expect(localStorage.getItem('akagi.settings.group')).toBe('view')
    fireEvent.click(screen.getByRole('tab', { name: '决策' }))
    fireEvent.click(screen.getByRole('tab', { name: '设置' }))
    expect(screen.getByRole('button', { name: '画面与窗口' }).getAttribute('aria-pressed')).toBe('true')
    unmount()
    render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '设置' }))
    expect(screen.getByRole('switch', { name: '窗口置顶' })).toBeTruthy()
  })

  it('has the switches of the window with their words', async () => {
    render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '设置' }))
    // Autoplay's level waits for the first snapshot: it is the core's to say. The bar has the same control.
    const off = () => within(screen.getAllByRole('radiogroup', { name: '自动操作' }).at(-1)!).getByRole('radio', { name: '关' }) as HTMLButtonElement
    await waitFor(() => expect(off().disabled).toBe(false))
    fireEvent.click(off())
    await waitFor(() => expect(fixture.calls).toContainEqual(['set_monitor_autoplay', { enabled: false }]))

    // The pointer's trail is the shell's to draw: the switch asks it, and shows what it saved.
    const trail = screen.getByRole('switch', { name: '光标轨迹' })
    expect(trail.getAttribute('aria-checked')).toBe('false')
    fireEvent.click(trail)
    await waitFor(() => expect(screen.getByRole('switch', { name: '光标轨迹' }).getAttribute('aria-checked')).toBe('true'))
    expect(fixture.calls).toContainEqual(['set_monitor_trail', { enabled: true }])

    const lean = screen.getByRole('switch', { name: '预推理' })
    fireEvent.click(lean)
    await waitFor(() => expect(screen.getByRole('switch', { name: '预推理' }).getAttribute('aria-checked')).toBe('true'))
    expect(fixture.calls).toContainEqual(['set_monitor_lean', { enabled: true }])

    // What autoplay presses between decisions: each its own switch, saved by the core.
    for (const [name, command] of [['快速确认', 'set_monitor_confirm'], ['立直自动和牌', 'set_monitor_auto_win']]) {
      const row = screen.getByRole('switch', { name })
      expect(row.getAttribute('aria-checked')).toBe('false')
      fireEvent.click(row)
      await waitFor(() => expect(screen.getByRole('switch', { name }).getAttribute('aria-checked')).toBe('true'))
      expect(fixture.calls).toContainEqual([command, { enabled: true }])
    }

    // The same two switches the bar has: one setting, two places to change it.
    group('画面与窗口')
    const pin = screen.getByRole('switch', { name: '窗口置顶' })
    expect(pin.getAttribute('aria-checked')).toBe('false')
    fireEvent.click(pin)
    await waitFor(() => expect(screen.getByRole('switch', { name: '窗口置顶' }).getAttribute('aria-checked')).toBe('true'))
    expect(fixture.calls).toContainEqual(['set_monitor_pinned', { pinned: true }])

    // The stream: off, its copy button is there but not shown; on, the address is the line under it.
    const copy = screen.getByRole('button', { name: '复制直播地址', hidden: true })
    expect(copy.hasAttribute('data-empty')).toBe(true)
    expect((copy as HTMLButtonElement).disabled).toBe(true)
    fireEvent.click(screen.getByRole('switch', { name: '直播输出' }))
    await waitFor(() => expect(screen.getByRole('switch', { name: '直播输出' }).getAttribute('aria-checked')).toBe('true'))
    expect(fixture.calls).toContainEqual(['set_monitor_stream', { enabled: true }])
    expect(screen.getByText('http://127.0.0.1:50123/0123456789abcdef01234567/')).toBeTruthy()
    const shown = screen.getByRole('button', { name: '复制直播地址' })
    expect(shown.hasAttribute('data-empty')).toBe(false)
    fireEvent.click(shown)
    await waitFor(() => expect(fixture.calls).toContainEqual(['copy_stream_url', undefined]))
    await waitFor(() => expect(shown.getAttribute('title')).toBe('已复制'))
    fireEvent.click(screen.getByRole('switch', { name: '直播输出' }))
    await waitFor(() => expect(screen.getByRole('switch', { name: '直播输出' }).getAttribute('aria-checked')).toBe('false'))
    expect(screen.queryByText(/127\.0\.0\.1/)).toBeNull()
  })
})

describe('another language', () => {
  const chinese = /[\u3400-\u9fff]/
  beforeEach(() => {
    fixture.snapshot = state(); fixture.options = { always_on_top: false }; fixture.calls = []; fixture.handEvents = []
    fixture.history = [{ id: 'G', ended_at: '2026-01-01T00:00:00Z', platform: 'majsoul', num_players: 4, our_seat: 0, final_scores: [25000, 25000, 25000, 25000],
      our_rank: 1, our_rank_points: 80, stats: { round: 10, agari: 3, houjuu: 1, riichi: 2 }, yaku: { 2: 2, 12: 1, 99: 1 }, match_info: { platform: 'majsoul', mode_id: 9 } }]
    const draw = { kind: 'yao9', hands: [{ seat: 1, tiles: ['1m', '9m', 'E'], winning_tile: null, target: null, ura_markers: [] }], scores: [25000, 25000, 25000, 25000], deltas: [0, 0, 0, 0] }
    fixture.snapshot.hands = [
      { round: 1, bakaze: 'E', kyoku: 1, honba: 0, result: draw, events: 12 },
      { round: 2, bakaze: 'E', kyoku: 1, honba: 1, result: null, events: 2 },
    ]
    fixture.snapshot.events = [
      { seq: 90, ts_ms: 0, event: { type: 'reach', actor: 2 } },
      { seq: 91, ts_ms: 0, event: { type: 'dahai', actor: 2, pai: '5pr', tsumogiri: false } },
      { seq: 92, ts_ms: 0, event: { type: 'pon', actor: 3, target: 2, pai: 'C', consumed: ['C', 'C'] } },
    ]
  })
  afterEach(() => setLang('zh-CN'))

  it('leaves no Chinese on any tab in English', async () => {
    setLang('en')
    const { container } = render(<Monitor />)
    // What is on screen and what is read out, the language list aside: each language is under its own name.
    const shown = () => {
      const copy = container.cloneNode(true) as HTMLElement
      copy.querySelectorAll('select[aria-label="Language"]').forEach(el => el.remove())
      return [copy.textContent ?? '', ...[...copy.querySelectorAll('*')].flatMap(el => ['title', 'aria-label', 'alt'].map(name => el.getAttribute(name) ?? ''))].join('\n')
    }
    const tabs = screen.getAllByRole('tab').map(tab => tab.textContent ?? '')
    expect(tabs).toEqual(['Decision', 'Hand', 'Game', 'Stats', 'Settings'])
    for (const name of tabs) {
      fireEvent.click(screen.getByRole('tab', { name }))
      await waitFor(() => expect(fixture.calls.some(([cmd]) => cmd === 'get_monitor_options')).toBe(true))
      const left = shown().split('\n').filter(line => chinese.test(line))
      expect(left, `on the ${name} tab`).toEqual([])
    }
    // Every group of the settings, the links among them.
    for (const name of ['Display & window', 'Link', 'Model & service', 'App']) {
      fireEvent.click(screen.getByRole('button', { name }))
      expect(shown().split('\n').filter(line => chinese.test(line)), `in ${name}`).toEqual([])
    }
    // An earlier hand's end, in the game tab.
    fireEvent.click(screen.getByRole('tab', { name: 'Game' }))
    fireEvent.click(screen.getAllByRole('button', { name: /^E1/ })[0])
    expect(shown().split('\n').filter(line => chinese.test(line))).toEqual([])
    // The record's own words too, once it has been read.
    fireEvent.click(screen.getByRole('tab', { name: 'Stats' }))
    expect(await screen.findByText('Riichi')).toBeTruthy()
    expect(shown().split('\n').filter(line => chinese.test(line))).toEqual([])
  })

  it('is chosen on the settings tab, changes every word at once, and is remembered', async () => {
    render(<Monitor />)
    fireEvent.click(screen.getByRole('tab', { name: '设置' }))
    fireEvent.click(screen.getByRole('button', { name: '画面与窗口' }))
    fireEvent.change(screen.getByRole('combobox', { name: '界面语言' }), { target: { value: 'ja' } })
    // The tab stays where it was; its name and everything on it are in the new language.
    expect(screen.getByRole('tab', { name: '設定' }).getAttribute('aria-selected')).toBe('true')
    expect(screen.queryByRole('tab', { name: '设置' })).toBeNull()
    expect(localStorage.getItem('akagi.language')).toBe('ja')
    expect(document.querySelector('.mon')?.getAttribute('data-lang')).toBe('ja')
  })
})
