import { useEffect, useRef, useState, type FormEvent } from 'react'
import { invoke, listen } from '@/lib/host'
import { LANGS, setLang, tr, useLang, type Lang } from '@/lib/i18n'
import type { AccountProfile, ProfileClient, ProfileState } from '@/lib/profiles'
import './Monitor.css'
import './Profiles.css'

type Editor = { id: string | null; name: string; platform: AccountProfile['platform'] }

// Profile-runtime words stay with this view; the monitor's policy/health
// vocabulary and its independent state are unchanged.
const CLIENT_TEXT: Record<Lang, Record<'starting' | 'active' | 'closing' | 'failed' | 'locked' | 'show' | 'multiple', string>> = {
  'zh-CN': { starting: '正在打开…', active: '已打开', closing: '正在清理并关闭…', failed: '运行失败', locked: '清理未确认，此档案已锁定', show: '显示', multiple: '可同时打开多个账号；运行时关闭此窗仅最小化' },
  'zh-TW': { starting: '正在開啟…', active: '已開啟', closing: '正在清理並關閉…', failed: '執行失敗', locked: '清理未確認，此檔案已鎖定', show: '顯示', multiple: '可同時開啟多個帳號；執行時關閉此窗僅最小化' },
  en: { starting: 'Opening…', active: 'Open', closing: 'Cleaning up…', failed: 'Client failed', locked: 'Cleanup unconfirmed; profile locked', show: 'Show', multiple: 'Multiple clients; close minimizes while clients run' },
  ja: { starting: '起動中…', active: '起動済み', closing: '終了処理中…', failed: '動作失敗', locked: '終了未確認・このプロファイルはロック中', show: '表示', multiple: '複数起動可・クライアント起動中は閉じると最小化' },
}

const locked = (client: ProfileClient | undefined) => client?.state === 'closing' || client?.state === 'failed' && client.cleanup === 'unconfirmed'

export function Profiles() {
  const lang = useLang()
  const [state, setState] = useState<ProfileState | null>(null)
  const [error, setError] = useState('')
  const [pending, setPending] = useState<Set<string>>(() => new Set())
  const [editor, setEditor] = useState<Editor | null>(null)
  const [formError, setFormError] = useState('')
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
  const blocked = busy || !state
  const words = CLIENT_TEXT[lang]
  const openEditor = (profile: AccountProfile | null) => {
    setFormError('')
    setEditor({ id: profile?.id ?? null, name: profile?.name ?? '', platform: profile?.platform ?? 'Majsoul' })
  }
  const request = async (method: string, args: Record<string, unknown>) => {
    const key = method === 'open_profile' ? `open:${String(args.id)}` : 'editor'
    setPending(current => new Set(current).add(key)); setError(''); setFormError('')
    try {
      const next = await invoke<ProfileState>(method, args)
      if (next.revision >= revision.current) { revision.current = next.revision; setState(next) }
      if (key === 'editor') setEditor(null)
    } catch (cause) {
      if (key === 'editor') setFormError(String(cause))
      else setError(String(cause))
    } finally { setPending(current => { const next = new Set(current); next.delete(key); return next }) }
  }
  const save = (event: FormEvent) => {
    event.preventDefault()
    if (!editor || busy || !editor.name.trim()) return
    void request(editor.id ? 'rename_profile' : 'create_profile', editor.id ? { id: editor.id, name: editor.name } : { name: editor.name, platform: editor.platform })
  }
  const status = error || state?.error || (!state ? tr('正在读取账号档案…') : '')

  return <main className="mon accounts" data-lang={lang}>
    <div className="accounts-canvas">
    <header className="accounts-bar">
      <div><h1>Washizu</h1><span>{tr('账号档案')}</span></div>
      <div className="accounts-tools">
        <select aria-label={tr('界面语言')} value={lang} onChange={event => setLang(event.target.value as Lang)}>{LANGS.map(([value, name]) => <option key={value} value={value}>{name}</option>)}</select>
        <button className="mbtn mbtn--primary accounts-create" id="new" disabled={!!blocked} onClick={() => openEditor(null)}>{tr('新建档案')}</button>
      </div>
    </header>
    <div className="accounts-status" title={status} data-error={!!(error || state?.error)} role={error || state?.error ? 'alert' : 'status'}>{status}</div>
    <section className="mstat accounts-panel" aria-label={tr('已保存的账号档案')}>
      <table>
        <colgroup><col /><col className="accounts-platform" /><col className="accounts-actions" /></colgroup>
        <thead><tr><th>{tr('名称')}</th><th>{tr('平台')}</th><th className="accounts-action-heading">{tr('操作')}</th></tr></thead>
        <tbody>{state?.profiles.map(profile => {
          const client = state.clients.find(client => client.id === profile.id)
          const label = client ? locked(client) && client.state === 'failed' ? words.locked : words[client.state] : ''
          return <tr key={profile.id} data-client-state={client?.state ?? 'closed'}>
          <td><span className="accounts-name" title={profile.name}>{profile.name}</span><span className="accounts-meta">{client && <span className="accounts-client" data-error={client.state === 'failed'} role={client.state === 'failed' ? 'alert' : undefined} title={client.error || label}>{label}{client.error ? ` · ${client.error}` : ''}</span>}{profile.id === 'existing' && <span className="accounts-original">{tr('原有资料')}</span>}{profile.lastOpened && <span title={new Date(profile.lastOpened).toLocaleString(lang)}>{tr('最近打开')} {new Date(profile.lastOpened).toLocaleDateString(lang)}</span>}</span></td>
          <td>{profile.platform === 'Tenhou' ? tr('天凤') : tr('雀魂')}</td>
          <td><div className="accounts-row-tools">
            <button className="mbtn accounts-rename" disabled={!!blocked} aria-label={tr('改名 {0}', profile.name)} onClick={() => openEditor(profile)}>{tr('改名')}</button>
            <button className="mbtn mbtn--primary" disabled={pending.has(`open:${profile.id}`) || !!locked(client)} aria-label={tr('打开 {0}', profile.name)} onClick={() => void request('open_profile', { id: profile.id })}>{client?.state === 'active' ? words.show : tr('打开')}</button>
          </div></td>
        </tr>})}</tbody>
      </table>
      {state?.profiles.length === 0 && <p className="accounts-empty">{tr('新建一份档案，选择平台后登录账号。')}</p>}
    </section>
    <footer className="accounts-footer"><span>{state ? tr('共 {0} 份档案', state.profiles.length) : ''}</span><span title={words.multiple}>{words.multiple}</span></footer>
    <dialog className="accounts-editor" id="editor" ref={dialog} aria-labelledby="editor-title" onCancel={() => setEditor(null)}>
      <form id="form" onSubmit={save}>
        <h2 id="editor-title">{editor?.id ? tr('修改档案名称') : tr('新建档案')}</h2>
        <label>{tr('档案名称')}<input id="name" ref={nameInput} value={editor?.name ?? ''} onChange={event => setEditor(current => current && { ...current, name: event.target.value })} required maxLength={48} autoComplete="off" /></label>
        <label className="accounts-platform-field" hidden={!!editor?.id}>{tr('平台')}<select id="platform" disabled={!!editor?.id} value={editor?.platform ?? 'Majsoul'} onChange={event => setEditor(current => current && { ...current, platform: event.target.value as AccountProfile['platform'] })}><option value="Majsoul">{tr('雀魂')}</option><option value="Tenhou">{tr('天凤')}</option></select></label>
        <div className="accounts-form-status" role="alert">{formError}</div>
        <div className="accounts-editor-actions"><button className="mbtn" type="button" disabled={busy} onClick={() => setEditor(null)}>{tr('取消')}</button><button className="mbtn mbtn--primary" id="save" type="submit" disabled={busy || !editor?.name.trim()}>{tr('保存')}</button></div>
      </form>
    </dialog>
    </div>
  </main>
}
