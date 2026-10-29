// @ts-check
// The actual settings UI in a hidden fixture window; no game input or accounts.
import { BrowserWindow, ipcMain } from 'electron'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { ProfileStore } from '../src/profiles.js'
import { registerDockScheme, serveDock } from '../src/dock.js'
registerDockScheme()

/** @param {string} root */
export async function renderProxySettings(root) {
  const store = new ProfileStore(path.join(root, 'accounts')).load()
  const main = store.create('Main', 'Majsoul'); store.create('Tenhou', 'Tenhou'); store.create('Other', 'Majsoul')
  let revision = 0
  const state = () => ({ revision, profiles: store.list(), clients: [], defaults: store.defaults(), error: '' })
  serveDock(fileURLToPath(new URL('../../frontend/dist', import.meta.url)))
  const window = new BrowserWindow({ width: 640, height: 368, useContentSize: true, show: false, webPreferences: {
    preload: fileURLToPath(new URL('../src/profiles-preload.cjs', import.meta.url)), sandbox: true, contextIsolation: true,
  } })
  ipcMain.handle('washizu:profiles', (event, method, args) => {
    assert.equal(event.sender, window.webContents)
    if (method === 'save_profile') { store.update(args.id, args.name, args.proxy); revision++ }
    else assert.equal(method, 'list_profiles', 'The settings fixture never opens a game')
    return { ok: true, value: state() }
  })
  const evaluate = (/** @type {string} */ code) => window.webContents.executeJavaScript(code)
  /** @param {string} expression */
  const ready = async expression => {
    for (let i = 0; i < 100; i++) { if (await evaluate(expression)) return; await new Promise(done => setTimeout(done, 20)) }
    throw new Error('Settings UI did not finish rendering')
  }
  try {
    for (const [width, height] of [[560, 304], [640, 368]]) for (const lang of ['zh-CN', 'zh-TW', 'en', 'ja']) {
      window.setContentSize(width, height)
      await window.loadURL('app://dock/index.html?view=profiles')
      await evaluate(`localStorage.setItem('akagi.language',${JSON.stringify(lang)})`)
      await window.loadURL('app://dock/index.html?view=profiles')
      await ready('document.querySelectorAll(".accounts-rename").length === 3')
      const aligned = await evaluate(`(()=>{const a=document.querySelectorAll('.accounts-tools button'),b=document.querySelectorAll('.accounts-row-tools button');return [...a].every((x,i)=>Math.abs(x.getBoundingClientRect().left-b[i].getBoundingClientRect().left)<=1)})()`)
      assert(aligned, 'Toolbar and account actions share the same grid')
      await evaluate('document.querySelector(".accounts-rename").click()')
      await ready('document.querySelector("dialog").open')
      const geometry = await evaluate(`(()=>{const d=document.querySelector('dialog'),b=d.querySelector('#save'),f=d.querySelector('.accounts-editor-fields'),r=b.getBoundingClientRect();return {fits:r.bottom<=innerHeight&&r.top>=0&&d.getBoundingClientRect().top>=0,overflow:f.scrollWidth>f.clientWidth||document.documentElement.scrollWidth>innerWidth||d.scrollHeight>d.clientHeight,mode:d.querySelector('fieldset select').value}})()`)
      assert(geometry.fits && !geometry.overflow, `${lang} ${width}x${height}: fields scroll internally and Save remains visible`)
      assert.equal(geometry.mode, 'system')
      if (lang === 'zh-CN' && width === 640) fs.writeFileSync(path.join(root, 'management.png'), (await window.webContents.capturePage()).toPNG())
    }
    assert.deepEqual(store.get(main.id).proxy, { mode: 'system', server: '' })
    console.log('PASS actual management UI geometry: 4 languages x 2 guide sizes; no game opened')
  } finally { ipcMain.removeHandler('washizu:profiles'); window.destroy() }
}
