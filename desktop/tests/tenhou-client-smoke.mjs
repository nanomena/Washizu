// Actual current Tenhou client + actual core interception. A hidden, isolated
// blank browser with bot and automatic input disabled; no account is loaded.
// electron tests/tenhou-client-smoke.mjs --core=<absolute executable>
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { app, BrowserWindow } from 'electron'
import { Core } from '../src/core.js'

const executable = process.argv.find(value => value.startsWith('--core='))?.slice(7)
assert.ok(executable && path.isAbsolute(executable) && fs.existsSync(executable), 'provide --core=<absolute executable>')
const corePath = executable
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-tenhou-client-'))
const browser = path.join(root, 'browser')
fs.mkdirSync(browser)
app.setPath('userData', browser)
app.commandLine.appendSwitch('remote-debugging-port', '0')
app.commandLine.appendSwitch('remote-debugging-address', '127.0.0.1')
fs.writeFileSync(path.join(root, 'config.toml'), '[platform]\nkind="Tenhou"\n[capture]\nurl="https://tenhou.net/4/"\n[bot]\nenabled=false\n[autoplay]\nenabled=false\n')
/** @param {number} ms */
const sleep = ms => new Promise(done => setTimeout(done, ms))
/** @param {() => Promise<any>} read @param {(value: any) => boolean} matches @param {string} name */
async function until(read, matches, name) {
  for (let attempt = 0; attempt < 240; attempt++) {
    const value = await read()
    if (matches(value)) return value
    await sleep(250)
  }
  throw new Error(`timed out: ${name}`)
}
/** @type {Core | undefined} */
let core
/** @type {BrowserWindow | undefined} */
let window
/** @type {string[]} */
const scriptErrors = []
// Return from Electron's ESM entry before waiting for its ready event.
async function main() {
try {
  await app.whenReady()
  const game = new BrowserWindow({ show: false, webPreferences: { sandbox: true, contextIsolation: true, nodeIntegration: false, backgroundThrottling: false } })
  window = game
  game.webContents.on('console-message', details => { if (details.level === 'error') scriptErrors.push(details.message) })
  await game.loadURL('about:blank')
  const activePort = await until(async () => {
    try { return Number(fs.readFileSync(path.join(browser, 'DevToolsActivePort'), 'utf8').split('\n')[0]) } catch { return null }
  }, value => Number.isInteger(value) && value > 0, 'isolated CDP listener')
  const targets = await fetch(`http://127.0.0.1:${activePort}/json/list`).then(response => response.json())
  const target = targets.find((/** @type {any} */ value) => value.type === 'page' && value.url === 'about:blank')
  assert.ok(target, 'hidden blank page exists')
  core = new Core(corePath, ['--home', root])
  assert.equal((await core.ready).platform, 'Tenhou')
  await core.call('open_game', { port: activePort, target: target.id })
  const observed = await until(async () => {
    try {
      return await game.webContents.executeJavaScript('({url:location.href, ready:document.readyState, discard:typeof window.__akagiTenhou?.R?.c21})')
    } catch { return null }
  }, value => value?.discard === 'function', 'current client instrumented by actual core')
  assert.equal(game.isVisible(), false, 'qualification never presents a window')
  assert.equal(observed.url, 'https://tenhou.net/4/')
  const monitor = /** @type {{autoplay_enabled: boolean, events: unknown[]}} */ (await core.call('get_monitor'))
  assert.equal(monitor.autoplay_enabled, false)
  assert.equal(monitor.events.length, 0, 'no game events or login input')
  await core.stop()
  assert.equal(core.released, true)
  console.log(JSON.stringify({ currentClientBooted: true, actualCoreInterception: true, discardHandler: observed.discard, visibleWindow: false, loggedIn: false, automaticInput: false, modelLoaded: false, cleanupAcknowledged: true, scriptErrors }))
} catch (error) {
  console.error(error)
  if (core) console.error(core.stderr)
  process.exitCode = 1
} finally {
  if (core && !core.gone) await core.stop().catch(error => { console.error(error); process.exitCode = 1 })
  window?.destroy()
  // Chromium may still be flushing its isolated browser directory until exit.
  console.log(`isolated evidence home: ${root}`)
  app.exit(Number(process.exitCode) || 0)
}
}
void main()
