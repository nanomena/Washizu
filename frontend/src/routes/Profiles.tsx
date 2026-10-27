import { useEffect, useRef, useState, type CSSProperties, type FormEvent } from 'react'
import { useCanvasScale } from '@/hooks/useCanvasScale'
import { invoke, listen } from '@/lib/host'
import { LANGS, setLang, tr, useLang, type Lang } from '@/lib/i18n'
import type { AccountProfile, ProfileState } from '@/lib/profiles'
import './Monitor.css'
import './Profiles.css'

type Editor = { id: string | null; name: string; platform: AccountProfile['platform'] }

export function Profiles() {
  const lang = useLang()
  const scale = useCanvasScale()
  const [state, setState] = useState<ProfileState | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
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

  const blocked = busy || !state || state.active !== null
  const openEditor = (profile: AccountProfile | null) => {
    setFormError('')
    setEditor({ id: profile?.id ?? null, name: profile?.name ?? '', platform: profile?.platform ?? 'Majsoul' })
  }
  const request = async (method: string, args: Record<string, unknown>) => {
    setBusy(true); setError(''); setFormError('')
    try {
      const next = await invoke<ProfileState>(method, args)
      if (next.revision >= revision.current) { revision.current = next.revision; setState(next) }
      setEditor(null)
    } catch (cause) {
      if (editor) setFormError(String(cause))
      else setError(String(cause))
    } finally { setBusy(false) }
  }
  const save = (event: FormEvent) => {
    event.preventDefault()
    if (!editor || busy || !editor.name.trim()) return
    void request(editor.id ? 'rename_profile' : 'create_profile', editor.id ? { id: editor.id, name: editor.name } : { name: editor.name, platform: editor.platform })
  }
  const status = error || state?.error || (!state ? tr('正在读取账号档案…')
    : state.active ? state.ready ? tr('账号窗口已打开') : tr('正在打开账号档案…') : '')

  return <main className="mon accounts" data-lang={lang} style={{ '--k': scale } as CSSProperties}>
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
        <colgroup><col /><col className="accounts-platform" /><col className="accounts-last" /><col className="accounts-actions" /></colgroup>
        <thead><tr><th>{tr('名称')}</th><th>{tr('平台')}</th><th>{tr('最近打开')}</th><th className="accounts-action-heading">{tr('操作')}</th></tr></thead>
        <tbody>{state?.profiles.map(profile => <tr key={profile.id}>
          <td><span className="accounts-name" title={profile.name}>{profile.name}</span>{profile.id === 'existing' && <span className="accounts-original">{tr('原有资料')}</span>}</td>
          <td>{profile.platform === 'Tenhou' ? tr('天凤') : tr('雀魂')}</td>
          <td>{profile.lastOpened ? new Date(profile.lastOpened).toLocaleDateString(lang) : '—'}</td>
          <td><div className="accounts-row-tools">
            <button className="mbtn accounts-rename" disabled={!!blocked} aria-label={tr('改名 {0}', profile.name)} onClick={() => openEditor(profile)}>{tr('改名')}</button>
            <button className="mbtn mbtn--primary" disabled={!!blocked} aria-label={tr('打开 {0}', profile.name)} onClick={() => void request('open_profile', { id: profile.id })}>{tr('打开')}</button>
          </div></td>
        </tr>)}</tbody>
      </table>
      {state?.profiles.length === 0 && <p className="accounts-empty">{tr('新建一份档案，选择平台后登录账号。')}</p>}
    </section>
    <footer className="accounts-footer"><span>{state ? tr('共 {0} 份档案', state.profiles.length) : ''}</span><span>{tr('一次打开一个账号')}</span></footer>
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
