// The real chooser and account windows, with a process-only fake core.
// No game site is opened. Cookies use an offline .invalid origin.
// Run on Linux: xvfb-run -a node tests/profiles-smoke.mjs
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const HERE = path.dirname(fileURLToPath(import.meta.url))
const DESKTOP = path.dirname(HERE)
const electron = createRequire(import.meta.url)('electron')
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-profiles-smoke-'))
const root = path.join(work, 'data')
/** @typedef {{type: string, url: string, targetId: string}} Target */
/** @typedef {{profiles: import('../src/profiles.js').AccountProfile[], active: string | null, ready: boolean}} State */
/** @param {number} ms */
const sleep = ms => new Promise(done => setTimeout(done, ms))
/** @template T @param {() => Promise<T>} read @param {(value: T) => boolean} predicate @param {string} label @returns {Promise<T>} */
async function until(read, predicate, label) {
  for (let i = 0; i < 150; i++) {
    const value = await read()
    if (predicate(value)) return value
    await sleep(100)
  }
  throw new Error(`Timed out: ${label}`)
}

class DevTools {
  /** @param {WebSocket} socket */
  constructor(socket) {
    this.socket = socket; this.next = 0
    /** @type {Map<number, {done: (value: any) => void, fail: (error: Error) => void}>} */
    this.pending = new Map()
    socket.onmessage = event => {
      const message = JSON.parse(String(event.data))
      const request = this.pending.get(message.id)
      if (request) { this.pending.delete(message.id); message.error ? request.fail(new Error(JSON.stringify(message.error))) : request.done(message.result) }
    }
    socket.onclose = () => { for (const request of this.pending.values()) request.fail(new Error('DevTools closed')); this.pending.clear() }
  }
  /** @param {number} port */
  static async connect(port) {
    const version = await until(() => fetch(`http://127.0.0.1:${port}/json/version`).then(r => r.json(), () => null), value => !!value, 'DevTools port')
    const socket = new WebSocket(version.webSocketDebuggerUrl)
    await new Promise((done, fail) => { socket.onopen = done; socket.onerror = fail })
    return new DevTools(socket)
  }
  /** @param {string} method @param {Record<string, unknown>} [params] @param {string} [sessionId] @returns {Promise<any>} */
  call(method, params = {}, sessionId) {
    const id = ++this.next
    return new Promise((done, fail) => { this.pending.set(id, { done, fail }); this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) })) })
  }
  /** @param {(target: Target) => boolean} match @returns {Promise<string>} */
  async page(match) {
    const page = await until(async () => (await this.call('Target.getTargets')).targetInfos.find((/** @type {Target} */ t) => t.type === 'page' && match(t)), value => !!value, 'page target')
    return (await this.call('Target.attachToTarget', { targetId: page.targetId, flatten: true })).sessionId
  }
  /** @param {string} session @param {string} expression @returns {Promise<any>} */
  async evaluate(session, expression) {
    const result = await this.call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, session)
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text)
    return result.result.value
  }
  async closeBrowser() {
    await this.call('Browser.close').catch(() => {})
    this.socket.close()
  }
}

fs.mkdirSync(root)
const oldConfig = '[platform]\nkind = "Majsoul"\n# existing user settings\n[autoplay]\nenabled = false\n'
fs.writeFileSync(path.join(root, 'config.toml'), oldConfig)
fs.mkdirSync(path.join(root, 'history'))
fs.writeFileSync(path.join(root, 'history', 'keep.jsonl'), 'existing game records\n')
const fake = path.join(work, 'core.sh')
fs.writeFileSync(fake, `#!/bin/sh\nexec "${process.execPath}" "${path.join(HERE, 'stand-in-core.js')}" --mode monitor --delay-release "$@"\n`, { mode: 0o755 })
const port = await new Promise(done => { const server = net.createServer().listen(0, '127.0.0.1', () => { const p = /** @type {net.AddressInfo} */ (server.address()).port; server.close(() => done(p)) }) })
let diagnostic = ''
const env = { ...process.env }; delete env.ELECTRON_RUN_AS_NODE
const shell = spawn(electron, [DESKTOP, '--no-sandbox', `--remote-debugging-port=${port}`, `--data=${root}`, `--core=${fake}`, `--dock=${path.join(DESKTOP, '..', 'frontend', 'dist')}`], { env, stdio: ['ignore', 'pipe', 'pipe'] })
shell.stdout.on('data', data => { diagnostic += data }); shell.stderr.on('data', data => { diagnostic += data })
/** @type {DevTools | null} */
let browser = null
/** @type {DevTools | null} */
let worker = null
try {
  browser = await DevTools.connect(port)
  const chooserBrowser = browser
  const chooser = await chooserBrowser.page(t => t.url.includes('?view=profiles'))
  /** @param {string} expression */
  const evaluate = expression => chooserBrowser.evaluate(chooser, expression)
  const state = async () => /** @type {State} */ (await evaluate('window.akagi.invoke("list_profiles")'))
  /** @param {string} selector @param {string} value @param {'HTMLInputElement' | 'HTMLSelectElement'} kind */
  const field = async (selector, value, kind = 'HTMLInputElement') => evaluate(`{
    const input=document.querySelector(${JSON.stringify(selector)});
    Object.getOwnPropertyDescriptor(${kind}.prototype,'value').set.call(input,${JSON.stringify(value)});
    input.dispatchEvent(new Event(${JSON.stringify(kind === 'HTMLSelectElement' ? 'change' : 'input')},{bubbles:true}));
  }`)
  await until(state, s => s.profiles.length === 1, 'profile initial read')
  await until(() => evaluate('!!document.querySelector(".accounts-tools select") && !document.querySelector("#new").disabled'), ready => ready, 'React chooser ready')
  await field('.accounts-tools select', 'zh-CN', 'HTMLSelectElement')
  assert.equal((await state()).profiles.length, 1)
  assert.equal((await state()).profiles[0].id, 'existing')
  assert.equal((await browser.call('Target.getTargets')).targetInfos.some((/** @type {Target} */ t) => t.url.startsWith('http')), false)
  console.log('ok chooser adopts the old account without opening a game or starting a core')
  await evaluate('document.querySelector("#new").click()')
  await until(() => evaluate('document.querySelector("#editor").open'), open => open, 'create editor open')
  await field('#name', '天凤 · 主号', 'HTMLInputElement')
  await field('#platform', 'Tenhou', 'HTMLSelectElement')
  await evaluate('document.querySelector("#form").requestSubmit()')
  const created = await until(state, s => s.profiles.length === 2, 'create Tenhou profile')
  const tenhou = created.profiles.find(p => p.platform === 'Tenhou')
  assert.ok(tenhou)
  await until(() => evaluate('document.querySelector("#editor").open'), open => !open, 'create dialog close')
  await evaluate('document.querySelector(".accounts-rename").click()')
  await until(() => evaluate('document.querySelector("#editor").open'), open => open, 'rename editor open')
  await field('#name', '雀魂 · 主号', 'HTMLInputElement')
  await evaluate('document.querySelector("#form").requestSubmit()')
  await until(state, s => s.profiles[0].name === '雀魂 · 主号', 'rename existing account')
  await until(() => evaluate('document.querySelector("#editor").open'), open => !open, 'rename dialog close')
  assert.equal(fs.readFileSync(path.join(root, 'config.toml'), 'utf8'), oldConfig)
  assert.equal(fs.readFileSync(path.join(root, 'history', 'keep.jsonl'), 'utf8'), 'existing game records\n')
  console.log('ok create and rename work through the real UI; original settings and records are untouched')

  const origin = 'https://washizu-account-fixture.invalid/'
  /** @param {DevTools} tools @param {string} session */
  const cookie = async (tools, session) => (await tools.call('Network.getCookies', { urls: [origin] }, session)).cookies.find((/** @type {{name: string, value: string}} */ c) => c.name === 'profile_marker')?.value
  let currentHome = ''
  /** @param {string} id @param {string} home @param {string} platform */
  async function openAccount(id, home, platform) {
    currentHome = home
    const marker = path.join(home, 'fake-core-released'); if (fs.existsSync(marker)) fs.unlinkSync(marker)
    await evaluate(`window.akagi.invoke("open_profile", {id:${JSON.stringify(id)}})`)
    await until(state, s => s.active === id && s.ready, 'account window ready')
    const logfile = fs.readdirSync(path.join(home, 'logs')).filter(f => /^shell-.*\.log$/.test(f)).sort().at(-1)
    assert.ok(logfile)
    const logged = fs.readFileSync(path.join(home, 'logs', logfile), 'utf8')
    const endpoint = Number(logged.match(/local DevTools port (\d+)/)?.[1])
    assert.ok(endpoint)
    const tools = worker = await DevTools.connect(endpoint)
    const dock = await tools.page(t => t.url.startsWith('app://dock/'))
    await until(() => tools.evaluate(dock, 'window.akagi.invoke("get_monitor_options")'), options => options.platform === platform, 'profile platform')
    await until(() => tools.evaluate(dock, 'window.akagi.invoke("get_monitor")'), snapshot => snapshot.capture.state === 'running', 'fake capture')
    const game = await tools.page(t => t.url === 'about:blank')
    assert.equal((await tools.call('Target.getTargets')).targetInfos.some((/** @type {Target} */ t) => /^https?:/.test(t.url)), false)
    return { tools, game }
  }
  async function closeAccount() {
    assert.ok(worker)
    await worker.closeBrowser(); worker = null
    await until(state, s => s.active === null, 'account shutdown and return to chooser')
    assert.ok(fs.existsSync(path.join(currentHome, 'fake-core-released')), 'the chooser waited for the old core to release its account')
  }
  let account = await openAccount(tenhou.id, path.join(root, 'profiles', tenhou.id), 'Tenhou')
  assert.equal(await cookie(account.tools, account.game), undefined)
  assert.equal((await account.tools.call('Network.setCookie', { url: origin, name: 'profile_marker', value: 'tenhou', expires: Date.now() / 1000 + 86400 }, account.game)).success, true)
  await closeAccount()
  account = await openAccount('existing', root, 'Majsoul')
  assert.equal(await cookie(account.tools, account.game), undefined)
  await account.tools.call('Network.setCookie', { url: origin, name: 'profile_marker', value: 'majsoul', expires: Date.now() / 1000 + 86400 }, account.game)
  await closeAccount()
  account = await openAccount(tenhou.id, path.join(root, 'profiles', tenhou.id), 'Tenhou')
  assert.equal(await cookie(account.tools, account.game), 'tenhou')
  await closeAccount()
  account = await openAccount('existing', root, 'Majsoul')
  assert.equal(await cookie(account.tools, account.game), 'majsoul')
  await closeAccount()
  assert.equal(fs.readFileSync(path.join(root, 'config.toml'), 'utf8'), oldConfig)
  assert.equal(fs.readFileSync(path.join(root, 'history', 'keep.jsonl'), 'utf8'), 'existing game records\n')
  console.log('ok four real account-window starts preserve and isolate persistent cookies; all game pages stayed about:blank')
  for (const [width, height] of [[880, 660], [1200, 900], [1280, 960]]) {
    await browser.call('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false }, chooser)
    for (const language of ['zh-CN', 'zh-TW', 'en', 'ja']) {
      await field('.accounts-tools select', language, 'HTMLSelectElement')
      await until(() => evaluate('document.querySelector("main").dataset.lang'), lang => lang === language, 'language change')
      const layout = await evaluate(`new Promise(done => requestAnimationFrame(() => requestAnimationFrame(() => {
        const panel=document.querySelector('.accounts-panel'), row=document.querySelector('tbody tr');
        const button=document.querySelector('.accounts-create'), heading=document.querySelector('h1');
        done({ overflow:document.documentElement.scrollWidth>innerWidth || panel.scrollWidth>panel.clientWidth,
          rowHeight:row.getBoundingClientRect().height, grid:getComputedStyle(document.querySelector('.accounts-canvas')).display,
          canvas:document.querySelector('.accounts-canvas').getBoundingClientRect().toJSON(), font:getComputedStyle(heading).fontFamily,
          weight:getComputedStyle(heading).fontWeight, button:getComputedStyle(button).backgroundColor });
      })))`)
      assert.equal(layout.overflow, false, `${width}x${height} ${language}: horizontal overflow`)
      assert.equal(layout.grid, 'grid')
      assert.ok(Math.abs(layout.canvas.width - width) <= 1 && Math.abs(layout.canvas.height - height) <= 1, 'the full canvas must scale uniformly')
      assert.ok(Math.abs(layout.rowHeight - 64 * width / 1280) <= 1, `${width}x${height} ${language}: account rows must scale with the canvas`)
      assert.ok(layout.font.includes('Golos Text'))
      assert.equal(layout.weight, '500')
      assert.equal(layout.button, 'rgb(27, 131, 228)')
    }
  }
  console.log('ok 12 viewport size/language layouts: shared 4:3 canvas, Grid, uniform scaling, no overflow, Golos 500 and blue action buttons')
  await browser.call('Emulation.setDeviceMetricsOverride', { width: 1200, height: 900, deviceScaleFactor: 1, mobile: false }, chooser)
  await field('.accounts-tools select', 'zh-CN', 'HTMLSelectElement')
  await until(() => evaluate('document.querySelector("main").dataset.lang'), lang => lang === 'zh-CN', 'screenshot language')
  const screenshot = await browser.call('Page.captureScreenshot', { format: 'png' }, chooser)
  const output = path.join(DESKTOP, 'test-output'); fs.mkdirSync(output, { recursive: true })
  fs.writeFileSync(path.join(output, 'profiles.png'), Buffer.from(screenshot.data, 'base64'))
  console.log(`ok chooser screenshot: ${path.join(output, 'profiles.png')}`)
  await browser.closeBrowser(); browser = null
} catch (error) {
  console.error(error); console.error(diagnostic.slice(-4000)); process.exitCode = 1
} finally {
  const remainingWorker = /** @type {DevTools | null} */ (worker)
  if (remainingWorker) await remainingWorker.closeBrowser().catch(() => {})
  if (browser) await browser.closeBrowser().catch(() => {})
  shell.kill()
  await sleep(800)
  assert.ok(work.startsWith(path.join(os.tmpdir(), 'washizu-profiles-smoke-')))
  fs.rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 })
}
