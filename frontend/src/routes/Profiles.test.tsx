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
    if (method === 'create_profile') next.profiles.push({ id: 'new-id', name: String(args!.name), platform: args!.platform as 'Tenhou', lastOpened: null })
    if (method === 'rename_profile') next.profiles = next.profiles.map(p => p.id === args!.id ? { ...p, name: String(args!.name) } : p)
    if (method === 'open_profile' && !next.clients.some(c => c.id === args!.id)) next.clients.push({ id: String(args!.id), state: 'starting', cleanup: 'unconfirmed', error: '' })
    fixture.state = next
    return next
  },
}))
import { Profiles } from './Profiles'

beforeEach(() => {
  setLang('zh-CN')
  fixture.state = { revision: 1, profiles: [{ id: 'existing', name: 'Main', platform: 'Majsoul', lastOpened: null }], clients: [], error: '' }
  fixture.initial = null; fixture.calls = []; fixture.failure = ''
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function (this: HTMLDialogElement) { this.setAttribute('open', '') } })
  Object.defineProperty(HTMLDialogElement.prototype, 'close', { configurable: true, value: function (this: HTMLDialogElement) { this.removeAttribute('open') } })
})
afterEach(() => { cleanup(); Reflect.deleteProperty(HTMLDialogElement.prototype, 'showModal'); Reflect.deleteProperty(HTMLDialogElement.prototype, 'close'); vi.restoreAllMocks(); setLang('zh-CN') })
const ready = async () => waitFor(() => expect((screen.getByRole('button', { name: '新建档案' }) as HTMLButtonElement).disabled).toBe(false))

describe('account profiles', () => {
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
    fireEvent.click(screen.getByRole('button', { name: '改名 Main' }))
    const form = within(screen.getByRole('dialog'))
    expect((form.getByLabelText('平台') as HTMLSelectElement).disabled).toBe(true)
    fireEvent.change(form.getByLabelText('档案名称'), { target: { value: 'Renamed' } })
    fireEvent.click(form.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(screen.getByText('Renamed')).toBeTruthy())
    expect(fixture.calls).toContainEqual(['rename_profile', { id: 'existing', name: 'Renamed' }])
    expect(fixture.state!.profiles[0].platform).toBe('Majsoul')
  })
  it('opens different accounts independently and keeps create/rename available while one starts', async () => {
    fixture.state!.profiles.push({ id: 'second', name: 'Second', platform: 'Tenhou', lastOpened: null })
    render(<Profiles />); await ready()
    fireEvent.click(screen.getByRole('button', { name: '打开 Main' }))
    await waitFor(() => expect(screen.getByText('正在打开…')).toBeTruthy())
    expect((screen.getByRole('button', { name: '打开 Second' }) as HTMLButtonElement).disabled).toBe(false)
    expect((screen.getByRole('button', { name: '新建档案' }) as HTMLButtonElement).disabled).toBe(false)
    expect((screen.getByRole('button', { name: '改名 Main' }) as HTMLButtonElement).disabled).toBe(false)
    fireEvent.click(screen.getByRole('button', { name: '打开 Second' }))
    await waitFor(() => expect(fixture.state!.clients).toHaveLength(2))
  })
  it('shows active clients, locks only the failed/closing profile, and focuses through the same open command', async () => {
    fixture.state!.profiles.push({ id: 'second', name: 'Second', platform: 'Tenhou', lastOpened: null })
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
