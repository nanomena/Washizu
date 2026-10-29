import { useEffect, useRef, useState, type DragEvent, type FormEvent } from 'react'
import { invoke, listen } from '@/lib/host'
import { tr, useLang, type Lang } from '@/lib/i18n'
import type { AccountProfile, GameProxy, ProfileClient, ProfileDefaults, ProfileState } from '@/lib/profiles'
import { ProfileSettings } from '@/components/monitor/ProfileSettings'
import './Monitor.css'
import './Profiles.css'

type Editor = { id: string | null; name: string; platform: AccountProfile['platform']; proxy: GameProxy }

// Profile-runtime words stay with this view; the monitor's policy/health
// vocabulary and its independent state are unchanged.
const CLIENT_TEXT: Record<Lang, Record<'starting' | 'active' | 'closing' | 'failed' | 'locked' | 'show', string>> = {
  'zh-CN': { starting: '正在打开…', active: '已打开', closing: '正在清理并关闭…', failed: '运行失败', locked: '清理未确认，此档案已锁定', show: '切换到窗口' },
  'zh-TW': { starting: '正在開啟…', active: '已開啟', closing: '正在清理並關閉…', failed: '執行失敗', locked: '清理未確認，此檔案已鎖定', show: '切換至視窗' },
  en: { starting: 'Opening…', active: 'Open', closing: 'Cleaning up…', failed: 'Client failed', locked: 'Cleanup unconfirmed; profile locked', show: 'Switch to window' },
  ja: { starting: '起動中…', active: '起動済み', closing: '終了処理中…', failed: '動作失敗', locked: '終了未確認・このプロファイルはロック中', show: 'ウィンドウへ' },
}

const locked = (client: ProfileClient | undefined) => client?.state === 'closing' || client?.state === 'failed' && client.cleanup === 'unconfirmed'

export function Profiles() {
  const lang = useLang()
  const [state, setState] = useState<ProfileState | null>(null)
  const [error, setError] = useState('')
  const [pending, setPending] = useState<Set<string>>(() => new Set())
  const [editor, setEditor] = useState<Editor | null>(null)
  const [formError, setFormError] = useState('')
  const [preferences, setPreferences] = useState<ProfileDefaults | null>(null)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [dragging, setDragging] = useState<string | null>(null)
  const [dropTarget, setDropTarget] = useState<{ id: string; side: 'before' | 'after' } | null>(null)
  const drag = useRef<{ id: string; ids: string[] } | null>(null)
  const dragAllowed = useRef(true)
  const revision = useRef(-1)
  const dialog = useRef<HTMLDialogElement>(null)
  const nameInput = useRef<HTMLInputElement>(null)

  useEffect(() => {
    let live = true
    const accept = (next: ProfileState) => {
      if (!live || next.revision < revision.current) return
      revision.current = next.revision
      setState(next)
    }
    const stop = listen<ProfileState>('profiles-updated', accept)
    void invoke<ProfileState>('list_profiles').then(accept, cause => { if (live) setError(String(cause)) })
    return () => { live = false; stop() }
  }, [])

  useEffect(() => {
    if (editor && !dialog.current?.open) { dialog.current?.showModal(); nameInput.current?.focus() }
    if (!editor && dialog.current?.open) dialog.current.close()
  }, [editor])

  const busy = pending.has('editor')
  const blocked = busy || pending.has('order') || !state
  const words = CLIENT_TEXT[lang]
  const openEditor = (profile: AccountProfile | null) => {
    setFormError('')
    setConfirmDelete(false)
    setEditor({ id: profile?.id ?? null, name: profile?.name ?? '', platform: profile?.platform ?? 'Majsoul', proxy: profile ? { ...profile.proxy } : { mode: 'system', server: '' } })
  }
  const request = async (method: string, args: Record<string, unknown>) => {
    const key = method === 'open_profile' ? `open:${String(args.id)}` : method === 'reorder_profiles' ? 'order' : 'editor'
    setPending(current => new Set(current).add(key)); setError(''); setFormError('')
    try {
      const next = await invoke<ProfileState>(method, args)
      if (next.revision >= revision.current) { revision.current = next.revision; setState(next) }
      if (key === 'editor') { setEditor(null); setPreferences(null) }
    } catch (cause) {
      if (key === 'editor') setFormError(String(cause))
      else setError(String(cause))
    } finally { setPending(current => { const next = new Set(current); next.delete(key); return next }) }
  }
  const endDrag = () => { drag.current = null; dragAllowed.current = true; setDragging(null); setDropTarget(null) }
  const sideAt = (event: DragEvent<HTMLTableRowElement>) => {
    const rect = event.currentTarget.getBoundingClientRect()
    return event.clientY < rect.top + rect.height / 2 ? 'before' as const : 'after' as const
  }
  const drop = (event: DragEvent<HTMLTableRowElement>, target: string) => {
    const moving = drag.current
    event.preventDefault(); endDrag()
    if (!moving || blocked || moving.id === target || !state) return
    const current = state.profiles.map(profile => profile.id)
    if (current.length !== moving.ids.length || current.some(id => !moving.ids.includes(id))) { setError(tr('账号列表已变化，请重新排序。')); return }
    const ids = current.filter(id => id !== moving.id), at = ids.indexOf(target)
    if (at < 0) return
    ids.splice(at + (sideAt(event) === 'after' ? 1 : 0), 0, moving.id)
    if (ids.some((id, i) => id !== current[i])) void request('reorder_profiles', { ids })
  }
  const save = (event: FormEvent) => {
    event.preventDefault()
    if (!editor || busy || !editor.name.trim()) return
    void request(editor.id ? 'save_profile' : 'create_profile', editor.id ? { id: editor.id, name: editor.name, proxy: editor.proxy } : { name: editor.name, platform: editor.platform })
  }
  const status = (preferences ? formError : '') || error || state?.error || (!state ? tr('正在读取账号档案…') : '')
  const editClient = state?.clients.find(client => client.id === editor?.id)
  const deleteBlocked = !!editClient && (editClient.state !== 'failed' || editClient.cleanup === 'unconfirmed')

  return <main className="mon accounts" data-lang={lang}>
    <div className="accounts-canvas">
    <header className="accounts-bar">
      <div><h1>Washizu</h1><span>{tr(preferences ? '设置' : '账号档案')}</span></div>
      <div className="accounts-tools">
        {preferences ? <><button className="mbtn" disabled={busy} onClick={() => setPreferences(null)}>{tr('取消')}</button><button className="mbtn mbtn--primary" disabled={busy} onClick={() => void request('save_profile_defaults', { defaults: preferences })}>{tr('保存')}</button></> : <><button className="mbtn" disabled={!!blocked} onClick={() => { setFormError(''); setPreferences(structuredClone(state!.defaults)) }}>{tr('设置')}</button><button className="mbtn mbtn--primary accounts-create" id="new" disabled={!!blocked} onClick={() => openEditor(null)}>{tr('新建档案')}</button></>}
      </div>
    </header>
    <div className="accounts-status" title={status} data-error={!!(error || state?.error)} role={error || state?.error ? 'alert' : 'status'}>{status}</div>
    {preferences ? <ProfileSettings defaults={preferences} onChange={busy ? null : setPreferences} /> : <section className="mstat accounts-panel" aria-label={tr('已保存的账号档案')}>
      <table>
        <colgroup><col /><col className="accounts-platform" /><col className="accounts-actions" /></colgroup>
        <thead><tr><th>{tr('名称')}</th><th>{tr('平台')}</th><th className="accounts-action-heading">{tr('操作')}</th></tr></thead>
        <tbody>{state?.profiles.map(profile => {
          const client = state.clients.find(client => client.id === profile.id)
          const label = client ? locked(client) && client.state === 'failed' ? words.locked : words[client.state] : ''
          return <tr key={profile.id} data-client-state={client?.state ?? 'closed'} draggable={!blocked}
            data-dragging={dragging === profile.id || undefined} data-drop={dropTarget?.id === profile.id ? dropTarget.side : undefined}
            onMouseDownCapture={event => { dragAllowed.current = !(event.target as HTMLElement).closest('button, input, select, textarea, a') }}
            onDragStart={event => {
              if (blocked || !dragAllowed.current || (event.target as HTMLElement).closest('button, input, select, textarea, a')) { event.preventDefault(); return }
              drag.current = { id: profile.id, ids: state!.profiles.map(p => p.id) }; setDragging(profile.id)
              event.dataTransfer.effectAllowed = 'move'; event.dataTransfer.setData('text/plain', profile.id)
            }}
            onDragOver={event => { if (drag.current && !blocked) { event.preventDefault(); event.dataTransfer.dropEffect = 'move'; setDropTarget(drag.current.id === profile.id ? null : { id: profile.id, side: sideAt(event) }) } }}
            onDrop={event => drop(event, profile.id)} onDragEnd={endDrag}>
          <td><span className="accounts-name" title={profile.name}>{profile.name}</span><span className="accounts-meta">{client && <span className="accounts-client" data-error={client.state === 'failed'} role={client.state === 'failed' ? 'alert' : undefined} title={client.error || label}>{label}{client.error ? ` · ${client.error}` : ''}</span>}{profile.id === 'existing' && <span className="accounts-original">{tr('原有资料')}</span>}{profile.lastOpened && <span title={new Date(profile.lastOpened).toLocaleString(lang)}>{tr('最近打开')} {new Date(profile.lastOpened).toLocaleDateString(lang)}</span>}</span></td>
          <td>{profile.platform === 'Tenhou' ? tr('天凤') : tr('雀魂')}</td>
          <td><div className="accounts-row-tools">
            <button className="mbtn accounts-rename" disabled={!!blocked} aria-label={tr('管理 {0}', profile.name)} onClick={() => openEditor(profile)}>{tr('管理')}</button>
            <button className="mbtn mbtn--primary" disabled={pending.has(`open:${profile.id}`) || !!locked(client)} aria-label={tr('打开 {0}', profile.name)} onClick={() => void request('open_profile', { id: profile.id })}>{client?.state === 'active' ? words.show : tr('打开')}</button>
          </div></td>
        </tr>})}</tbody>
      </table>
      {state?.profiles.length === 0 && <p className="accounts-empty">{tr('新建一份档案，选择平台后登录账号。')}</p>}
    </section>}
    <footer className="accounts-footer">{preferences ? <span>{tr('仅用于以后新建的档案，已有档案保持各自设置。')}</span> : <span>{state ? tr('共 {0} 份档案', state.profiles.length) : ''}</span>}</footer>
    <dialog className="accounts-editor" id="editor" ref={dialog} aria-labelledby="editor-title" onCancel={event => { if (busy) event.preventDefault(); else setEditor(null) }}>
      <form id="form" onSubmit={save}>
        <h2 id="editor-title">{editor?.id ? tr('管理档案') : tr('新建档案')}</h2>
        <div className="accounts-editor-fields">
        <label>{tr('档案名称')}<input id="name" ref={nameInput} value={editor?.name ?? ''} onChange={event => setEditor(current => current && { ...current, name: event.target.value })} required maxLength={48} autoComplete="off" /></label>
        {!editor?.id && <label>{tr('平台')}<select id="platform" value={editor?.platform ?? 'Majsoul'} onChange={event => setEditor(current => current && { ...current, platform: event.target.value as AccountProfile['platform'] })}><option value="Majsoul">{tr('雀魂')}</option><option value="Tenhou">{tr('天凤')}</option></select></label>}
        {editor?.id && <fieldset className="accounts-proxy" disabled={busy}>
          <legend>{tr('游戏代理')}</legend>
          <label>{tr('连接方式')}<select value={editor.proxy.mode} onChange={event => setEditor(current => current && { ...current, proxy: { ...current.proxy, mode: event.target.value as GameProxy['mode'] } })}>
            <option value="system">{tr('跟随系统')}</option><option value="direct">{tr('直连')}</option><option value="custom">{tr('指定代理')}</option>
          </select></label>
          <label>{tr('代理地址')}<input value={editor.proxy.server} disabled={editor.proxy.mode !== 'custom'} required={editor.proxy.mode === 'custom'} placeholder="socks5://127.0.0.1:1080" autoComplete="off" spellCheck={false} onChange={event => setEditor(current => current && { ...current, proxy: { ...current.proxy, server: event.target.value } })} /></label>
          <p>{tr('支持 HTTP、HTTPS、SOCKS5；仅用于游戏连接，下次打开此账号时生效。')}</p>
        </fieldset>}
        <div className="accounts-form-status" role="alert">{formError}</div>
        {editor?.id && <div className="accounts-delete">
          {confirmDelete ? <><p>{tr('删除后将从列表移除，本地登录资料和记录保留。确定删除「{0}」？', state?.profiles.find(p => p.id === editor.id)?.name ?? editor.name)}</p><button className="mbtn" type="button" disabled={busy} onClick={() => setConfirmDelete(false)}>{tr('取消')}</button><button className="mbtn accounts-danger" type="button" disabled={busy || deleteBlocked} onClick={() => void request('delete_profile', { id: editor.id })}>{tr('确认删除')}</button></> : <button className="mbtn accounts-danger" type="button" disabled={busy || deleteBlocked} onClick={() => setConfirmDelete(true)}>{tr('删除档案')}</button>}
          {deleteBlocked && <p>{tr('请先关闭此档案窗口，等待清理完成后再删除。')}</p>}
        </div>}
        </div>
        <div className="accounts-editor-actions"><button className="mbtn" type="button" disabled={busy} onClick={() => setEditor(null)}>{tr('取消')}</button><button className="mbtn mbtn--primary" id="save" type="submit" disabled={busy || !editor?.name.trim() || confirmDelete}>{tr('保存')}</button></div>
      </form>
    </dialog>

    </div>
  </main>
}
