import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { setLang } from '@/lib/i18n'
import type { ProfileState } from '@/lib/profiles'

const fixture = vi.hoisted(() => ({ state: null as ProfileState | null, initial: null as Promise<ProfileState> | null,
  listener: null as ((state: ProfileState) => void) | null, calls: [] as Array<[string, Record<string, unknown> | undefined]>, failure: '' }))
vi.mock('@/lib/host', () => ({
  listen: (_name: string, listener: (state: ProfileState) => void) => { fixture.listener = listener; return () => { fixture.listener = null } },
  invoke: async (method: string, args?: Record<string, unknown>) => {
    fixture.calls.push([method, args])
    if (method === 'list_profiles') return fixture.initial ?? fixture.state
    if (fixture.failure) throw fixture.failure
    const next = structuredClone(fixture.state!)
    next.revision++
    if (method === 'create_profile') next.profiles.push({ id: 'new-id', name: String(args!.name), platform: args!.platform as 'Tenhou', lastOpened: null, proxy: { mode: 'system', server: '' } })
    if (method === 'save_profile') next.profiles = next.profiles.map(p => p.id === args!.id ? { ...p, name: String(args!.name), proxy: args!.proxy as typeof p.proxy } : p)
    if (method === 'reorder_profiles') next.profiles = (args!.ids as string[]).map(id => next.profiles.find(p => p.id === id)!)
    if (method === 'open_profile' && !next.clients.some(c => c.id === args!.id)) next.clients.push({ id: String(args!.id), state: 'starting', cleanup: 'unconfirmed', error: '' })
    if (method === 'delete_profile') next.profiles = next.profiles.filter(p => p.id !== args!.id)
    if (method === 'save_profile_defaults') next.defaults = args!.defaults as ProfileState['defaults']
    fixture.state = next
    return next
  },
}))
import { Profiles } from './Profiles'

beforeEach(() => {
  setLang('zh-CN')
  fixture.state = { revision: 1, profiles: [{ id: 'existing', name: 'Main', platform: 'Majsoul', lastOpened: null, proxy: { mode: 'system', server: '' } }], clients: [], error: '', defaults: { Majsoul: { autoplay: 'play', quick_confirm: true, pointer_lean: true, pointer_trail: true, auto_win: true, obs_output: true }, Tenhou: { autoplay: 'continue', quick_confirm: true, pointer_lean: true, pointer_trail: true, auto_win: false, obs_output: true } } }
  fixture.initial = null; fixture.calls = []; fixture.failure = ''
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function (this: HTMLDialogElement) { this.setAttribute('open', '') } })
  Object.defineProperty(HTMLDialogElement.prototype, 'close', { configurable: true, value: function (this: HTMLDialogElement) { this.removeAttribute('open') } })
})
afterEach(() => { cleanup(); Reflect.deleteProperty(HTMLDialogElement.prototype, 'showModal'); Reflect.deleteProperty(HTMLDialogElement.prototype, 'close'); vi.restoreAllMocks(); setLang('zh-CN') })
const ready = async () => waitFor(() => expect((screen.getByRole('button', { name: '新建档案' }) as HTMLButtonElement).disabled).toBe(false))

describe('account profiles', () => {
  it('keeps language inside settings and saves per-platform new-profile defaults', async () => {
    render(<Profiles />); await ready()
    expect(screen.queryByRole('combobox', { name: '界面语言' })).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '设置' }))
    expect(screen.getByRole('combobox', { name: '界面语言' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '雀魂' }))
    expect(screen.getByRole('radio', { name: '打牌' }).getAttribute('aria-checked')).toBe('true')
    fireEvent.click(screen.getByRole('button', { name: '天凤' }))
    expect(screen.getByRole('radio', { name: '连战' }).getAttribute('aria-checked')).toBe('true')
    expect(screen.queryByRole('switch', { name: '客户端自动和牌' })).toBeNull()
    fireEvent.click(screen.getByRole('switch', { name: '光标轨迹' }))
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(screen.getByRole('button', { name: '新建档案' })).toBeTruthy())
    expect(fixture.state!.defaults.Tenhou.pointer_trail).toBe(false)
    expect(fixture.state!.defaults.Majsoul.pointer_trail).toBe(true)
  })
  it('confirms deletion and explains that local data is retained', async () => {
    render(<Profiles />); await ready()
    fireEvent.click(screen.getByRole('button', { name: '管理 Main' }))
    fireEvent.click(screen.getByRole('button', { name: '删除档案' }))
    expect(screen.getByText(/本地登录资料和记录保留/)).toBeTruthy()
    expect(fixture.calls.some(([method]) => method === 'delete_profile')).toBe(false)
    fireEvent.click(screen.getByRole('button', { name: '确认删除' }))
    await waitFor(() => expect(screen.queryByText('Main')).toBeNull())
    expect(fixture.calls).toContainEqual(['delete_profile', { id: 'existing' }])
  })
  it('does not offer deletion of a running or cleanup-unconfirmed profile', async () => {
    fixture.state!.clients = [{ id: 'existing', state: 'active', cleanup: 'unconfirmed', error: '' }]
    render(<Profiles />); await ready()
    fireEvent.click(screen.getByRole('button', { name: '管理 Main' }))
    expect((screen.getByRole('button', { name: '删除档案' }) as HTMLButtonElement).disabled).toBe(true)
  })
  it('shows adopted data without inventing a logged-in or model-ready status', async () => {
    render(<Profiles />); await ready()
    expect(screen.getByText('Main')).toBeTruthy()
    expect(screen.getByText('原有资料')).toBeTruthy()
    expect(screen.queryByText('已登录')).toBeNull()
    expect(fixture.calls.map(([method]) => method)).toEqual(['list_profiles'])
  })
  it('creates a named Tenhou account through the shared shell bridge', async () => {
    render(<Profiles />); await ready()
    fireEvent.click(screen.getByRole('button', { name: '新建档案' }))
    const form = within(screen.getByRole('dialog'))
    fireEvent.change(form.getByLabelText('档案名称'), { target: { value: 'Tenhou Main' } })
    fireEvent.change(form.getByLabelText('平台'), { target: { value: 'Tenhou' } })
    fireEvent.click(form.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(screen.getByText('Tenhou Main')).toBeTruthy())
    expect(fixture.calls).toContainEqual(['create_profile', { name: 'Tenhou Main', platform: 'Tenhou' }])
  })
  it('renames a profile without changing its id or platform', async () => {
    render(<Profiles />); await ready()
    fireEvent.click(screen.getByRole('button', { name: '管理 Main' }))
    const form = within(screen.getByRole('dialog'))
    expect(form.queryByLabelText('平台')).toBeNull()
    fireEvent.change(form.getByLabelText('档案名称'), { target: { value: 'Renamed' } })
    fireEvent.click(form.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(screen.getByText('Renamed')).toBeTruthy())
    expect(fixture.calls).toContainEqual(['save_profile', { id: 'existing', name: 'Renamed', proxy: { mode: 'system', server: '' } }])
    expect(fixture.state!.profiles[0].platform).toBe('Majsoul')
  })
  it('opens different accounts independently and keeps create/rename available while one starts', async () => {
    fixture.state!.profiles.push({ id: 'second', name: 'Second', platform: 'Tenhou', lastOpened: null, proxy: { mode: 'system', server: '' } })
    render(<Profiles />); await ready()
    fireEvent.click(screen.getByRole('button', { name: '打开 Main' }))
    await waitFor(() => expect(screen.getByText('正在打开…')).toBeTruthy())
    expect((screen.getByRole('button', { name: '打开 Second' }) as HTMLButtonElement).disabled).toBe(false)
    expect((screen.getByRole('button', { name: '新建档案' }) as HTMLButtonElement).disabled).toBe(false)
    expect((screen.getByRole('button', { name: '管理 Main' }) as HTMLButtonElement).disabled).toBe(false)
    fireEvent.click(screen.getByRole('button', { name: '打开 Second' }))
    await waitFor(() => expect(fixture.state!.clients).toHaveLength(2))
  })
  it('saves only the managed account proxy without reopening or changing a running account', async () => {
    fixture.state!.profiles.push({ id: 'second', name: 'Second', platform: 'Tenhou', lastOpened: null, proxy: { mode: 'direct', server: '' } })
    fixture.state!.clients = [{ id: 'existing', state: 'active', cleanup: 'unconfirmed', error: '' }]
    render(<Profiles />); await ready()
    fireEvent.click(screen.getByRole('button', { name: '管理 Main' }))
    const form = within(screen.getByRole('dialog'))
    expect((form.getByLabelText('代理地址') as HTMLInputElement).disabled).toBe(true)
    fireEvent.change(form.getByLabelText('连接方式'), { target: { value: 'custom' } })
    fireEvent.change(form.getByLabelText('代理地址'), { target: { value: 'socks5://127.0.0.1:1080' } })
    fireEvent.click(form.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(fixture.state!.profiles[0].proxy.mode).toBe('custom'))
    expect(fixture.calls).toContainEqual(['save_profile', { id: 'existing', name: 'Main', proxy: { mode: 'custom', server: 'socks5://127.0.0.1:1080' } }])
    expect(fixture.state!.profiles[1].proxy).toEqual({ mode: 'direct', server: '' })
    expect(fixture.state!.clients[0].state).toBe('active')
    expect(fixture.calls.some(([method]) => method === 'open_profile')).toBe(false)
    fireEvent.click(screen.getByRole('button', { name: '管理 Main' }))
    expect((within(screen.getByRole('dialog')).getByLabelText('代理地址') as HTMLInputElement).value).toBe('socks5://127.0.0.1:1080')
  })
  it('moves an entire running account row and saves stable ids while its buttons remain clickable', async () => {
    fixture.state!.profiles.push({ id: 'second', name: 'Second', platform: 'Tenhou', lastOpened: null, proxy: { mode: 'direct', server: '' } })
    fixture.state!.clients = [{ id: 'existing', state: 'active', cleanup: 'unconfirmed', error: '' }]
    render(<Profiles />); await ready()
    const main = screen.getByText('Main').closest('tr')!, second = screen.getByText('Second').closest('tr')!
    const transfer = { effectAllowed: '', dropEffect: '', setData: vi.fn() }
    fireEvent.mouseDown(screen.getByText('Main'))
    fireEvent.dragStart(main, { dataTransfer: transfer })
    fireEvent.dragOver(second, { dataTransfer: transfer, clientY: 10 })
    fireEvent.drop(second, { dataTransfer: transfer, clientY: 10 })
    await waitFor(() => expect(fixture.calls).toContainEqual(['reorder_profiles', { ids: ['second', 'existing'] }]))
    expect(fixture.state!.clients[0].state).toBe('active')
    expect(fixture.state!.profiles[0].proxy).toEqual({ mode: 'direct', server: '' })
    const button = screen.getByRole('button', { name: '管理 Main' })
    fireEvent.mouseDown(button)
    expect(fireEvent.dragStart(button.closest('tr')!, { dataTransfer: transfer })).toBe(false)
    fireEvent.click(button)
    expect(screen.getByRole('dialog')).toBeTruthy()
    expect(fixture.calls.filter(([method]) => method === 'reorder_profiles')).toHaveLength(1)
  })
  it('shows active clients, locks only the failed/closing profile, and focuses through the same open command', async () => {
    fixture.state!.profiles.push({ id: 'second', name: 'Second', platform: 'Tenhou', lastOpened: null, proxy: { mode: 'system', server: '' } })
    fixture.state!.clients = [{ id: 'existing', state: 'failed', cleanup: 'unconfirmed', error: 'Cleanup ACK missing' }, { id: 'second', state: 'active', cleanup: 'unconfirmed', error: '' }]
    render(<Profiles />); await ready()
    expect(screen.getByRole('alert').textContent).toContain('Cleanup ACK missing')
    expect((screen.getByRole('button', { name: '打开 Main' }) as HTMLButtonElement).disabled).toBe(true)
    const show = screen.getByRole('button', { name: '打开 Second' }) as HTMLButtonElement
    expect(show.disabled).toBe(false); expect(show.textContent).toBe('切换到窗口')
    fireEvent.click(show)
    await waitFor(() => expect(fixture.calls).toContainEqual(['open_profile', { id: 'second' }]))
    await act(async () => fixture.listener!({ ...fixture.state!, revision: 8, clients: [{ id: 'second', state: 'closing', cleanup: 'acknowledged', error: '' }] }))
    expect((screen.getByRole('button', { name: '打开 Second' }) as HTMLButtonElement).disabled).toBe(true)
    expect((screen.getByRole('button', { name: '打开 Main' }) as HTMLButtonElement).disabled).toBe(false)
    expect(screen.getByText('正在清理并关闭…')).toBeTruthy()
  })
  it('does not let a late initial read replace a newer profile event', async () => {
    let resolve!: (state: ProfileState) => void
    const previous = structuredClone(fixture.state!)
    fixture.initial = new Promise(done => { resolve = done })
    render(<Profiles />)
    await act(async () => { fixture.listener!({ ...previous, revision: 3, profiles: [{ ...previous.profiles[0], name: 'Newer' }] }) })
    await act(async () => { resolve(previous) })
    expect(screen.getByText('Newer')).toBeTruthy()
    expect(screen.queryByText('Main')).toBeNull()
  })
  it('keeps a failed edit and its name visible for correction', async () => {
    render(<Profiles />); await ready()
    fireEvent.click(screen.getByRole('button', { name: '新建档案' }))
    const form = within(screen.getByRole('dialog'))
    fireEvent.change(form.getByLabelText('档案名称'), { target: { value: 'Retry' } })
    fixture.failure = 'Cannot save this profile'
    fireEvent.click(form.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(form.getByText('Cannot save this profile')).toBeTruthy())
    expect((form.getByLabelText('档案名称') as HTMLInputElement).value).toBe('Retry')
  })
  it('uses the same language selection as the monitor, including its editor', async () => {
    setLang('en')
    render(<Profiles />)
    await waitFor(() => expect((screen.getByRole('button', { name: 'New profile' }) as HTMLButtonElement).disabled).toBe(false))
    expect(screen.getByText('Account profile')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'New profile' }))
    const form = screen.getByRole('dialog')
    expect(form.textContent).toContain('Profile name')
    expect(form.textContent).not.toMatch(/[\u3400-\u9fff]/)
  })
})
