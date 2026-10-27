// @ts-check
// The chooser owns no game, core or model. Each account opens in a separate
// process so Chromium's userData is fixed before Electron becomes ready.
import { app, BrowserWindow, dialog, ipcMain, Menu, screen } from 'electron'
import path from 'node:path'
import fs from 'node:fs'
import { fileURLToPath } from 'node:url'
import { ProfileStore } from './profiles.js'
import { ProfileSession } from './profile-session.js'
import { registerDockScheme, serveDock } from './dock.js'
import { MIN, openingSize } from './layout.js'
import { holdRatio } from './window-frame.js'

const HERE = path.dirname(fileURLToPath(import.meta.url))

/** @param {string} root */
export async function startProfiles(root) {
  registerDockScheme()
  fs.mkdirSync(path.join(root, 'launcher', 'browser'), { recursive: true })
  app.setPath('userData', path.join(root, 'launcher', 'browser'))
  if (!app.requestSingleInstanceLock()) { app.exit(0); return }
  const store = new ProfileStore(root).load()
  const forwarded = ['core', 'dock', 'spout'].flatMap(name => {
    const value = app.commandLine.getSwitchValue(name)
    return value ? [`--${name}=${value}`] : []
  })
  if (app.commandLine.hasSwitch('no-sandbox')) forwarded.push('--no-sandbox')
  const session = new ProfileSession({ executable: process.execPath, entry: app.isPackaged ? [] : [path.resolve(HERE, '..')], root, options: forwarded })
  /** @type {BrowserWindow | null} */
  let window = null
  let error = ''
  let revision = 0
  const state = () => ({ revision, profiles: store.list(), active: session.active, ready: session.ready, error })
  const changed = () => { revision++; window?.webContents.send('akagi:event', 'profiles-updated', state()) }
  const show = () => { window?.show(); if (window?.isMinimized()) window.restore(); window?.focus() }
  app.on('second-instance', () => { if (session.child) session.focus(); else show() })
  app.on('activate', show)
  app.on('window-all-closed', () => { if (!session.child) app.quit() })
  app.on('before-quit', event => { if (session.child) { event.preventDefault(); session.focus() } })
  session.on('change', changed)
  session.on('ready', id => {
    try { store.opened(id) } catch (cause) { error = `最近使用时间未保存：${cause instanceof Error ? cause.message : String(cause)}` }
    window?.hide()
  })
  session.on('closed', result => { error = result.error; changed(); show() })
  ipcMain.handle('washizu:profiles', (event, method, args) => {
    try {
      if (!window || event.sender !== window.webContents) throw new Error('只有账号档案页可以执行此操作。')
      const values = args && typeof args === 'object' ? args : {}
      if (method !== 'list_profiles' && session.child) throw new Error('请先从当前窗口返回账号档案。')
      if (method === 'create_profile') store.create(values.name, values.platform)
      else if (method === 'rename_profile') store.rename(values.id, values.name)
      else if (method === 'open_profile') { const profile = store.get(values.id); error = ''; session.open(profile.id) }
      else if (method !== 'list_profiles') throw new Error('未知的档案操作。')
      if (method !== 'list_profiles') changed()
      return { ok: true, value: state() }
    } catch (cause) { return { ok: false, error: cause instanceof Error ? cause.message : String(cause) } }
  })
  await app.whenReady()
  Menu.setApplicationMenu(null)
  const files = path.resolve(app.commandLine.getSwitchValue('dock') || process.env.AKAGI_DOCK || (app.isPackaged ? path.join(process.resourcesPath, 'dock') : path.join(HERE, '..', '..', 'frontend', 'dist')))
  serveDock(files)
  const size = openingSize(screen.getPrimaryDisplay().workAreaSize)
  window = new BrowserWindow({ ...size, useContentSize: true, minWidth: MIN.width, minHeight: MIN.height, maximizable: false, fullscreenable: false, title: 'Washizu · 账号档案', backgroundColor: '#ece9e4', show: false,
    webPreferences: { preload: path.join(HERE, 'profiles-preload.cjs'), sandbox: true, contextIsolation: true, nodeIntegration: false } })
  holdRatio(window)
  window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  window.webContents.on('will-navigate', event => event.preventDefault())
  window.on('close', event => { if (session.child) { event.preventDefault(); session.focus(); window?.hide() } })
  window.on('closed', () => { window = null })
  window.once('ready-to-show', show)
  await window.loadURL('app://dock/index.html?view=profiles')
}

process.on('uncaughtException', error => { dialog.showErrorBox('Washizu 无法打开账号档案', error.message); app.exit(1) })
