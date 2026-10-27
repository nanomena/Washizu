// Real Electron processes under Xvfb, fake cores, temporary profiles, and
// about:blank game pages. No real game, user account, desktop input or model.
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
/** @typedef {{revision: number, profiles: import('../src/profiles.js').AccountProfile[], clients: import('../src/profile-session.js').ClientState[], defaults: import('../src/profile-defaults.js').ProfileDefaults, error: string}} State */
const sleep = (/** @type {number} */ ms) => new Promise(done => setTimeout(done, ms))
/** @template T @param {() => Promise<T>} read @param {(value: T) => boolean} predicate @param {string} label @returns {Promise<T>} */
async function until(read, predicate, label) {
  for (let i = 0; i < 180; i++) { const value = await read(); if (predicate(value)) return value; await sleep(100) }
  throw new Error(`Timed out: ${label}`)
}

class DevTools {
  /** @param {WebSocket} socket */
  constructor(socket) {
    this.socket = socket; this.next = 0
    /** @type {Map<number, {done: (value: any) => void, fail: (error: Error) => void, timer: NodeJS.Timeout}>} */
    this.pending = new Map()
    socket.onmessage = event => {
      const message = JSON.parse(String(event.data)), request = this.pending.get(message.id)
      if (request) { clearTimeout(request.timer); this.pending.delete(message.id); message.error ? request.fail(new Error(JSON.stringify(message.error))) : request.done(message.result) }
    }
    socket.onclose = () => { for (const request of this.pending.values()) { clearTimeout(request.timer); request.fail(new Error('DevTools closed')) } this.pending.clear() }
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
    return new Promise((done, fail) => {
      const timer = setTimeout(() => { this.pending.delete(id); fail(new Error(`DevTools timeout: ${method}`)) }, 15_000)
      this.pending.set(id, { done, fail, timer }); this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }))
    })
  }
  /** @param {(target: Target) => boolean} match */
  async page(match) {
    const target = await until(async () => (await this.call('Target.getTargets')).targetInfos.find((/** @type {Target} */ t) => t.type === 'page' && match(t)), value => !!value, 'page target')
    return (await this.call('Target.attachToTarget', { targetId: target.targetId, flatten: true })).sessionId
  }
  /** @param {string} session @param {string} expression */
  async evaluate(session, expression) {
    const result = await this.call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, session)
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text)
    return result.result.value
  }
  async close() { await this.call('Browser.close').catch(() => {}); this.socket.close() }
}

fs.mkdirSync(root)
const original = '[platform]\nkind = "Majsoul"\n# existing user settings\n[autoplay]\nenabled = false\n'
fs.writeFileSync(path.join(root, 'config.toml'), original)
fs.mkdirSync(path.join(root, 'history')); fs.writeFileSync(path.join(root, 'history', 'keep.jsonl'), 'existing game records\n')
const fake = path.join(work, 'core.sh')
fs.writeFileSync(fake, `#!/bin/sh\nexec "${process.execPath}" "${path.join(HERE, 'stand-in-core.js')}" --mode monitor --live-game --delay-release --release-delay 1200 "$@"\n`, { mode: 0o755 })
const port = await new Promise(done => { const probe = net.createServer().listen(0, '127.0.0.1', () => { const n = /** @type {net.AddressInfo} */ (probe.address()).port; probe.close(() => done(n)) }) })
const env = { ...process.env }; delete env.ELECTRON_RUN_AS_NODE
const shell = spawn(electron, [path.join(HERE, 'profiles-app.js'), '--no-sandbox', `--remote-debugging-port=${port}`, `--data=${root}`, `--core=${fake}`, `--dock=${path.join(DESKTOP, '..', 'frontend', 'dist')}`], { env, stdio: ['ignore', 'pipe', 'pipe', 'ipc'] })
let diagnostic = '', next = 0
assert.ok(shell.stdout && shell.stderr)
shell.stdout.on('data', data => { diagnostic += data }); shell.stderr.on('data', data => { diagnostic += data })
/** @param {string} action @param {string} [profile] @returns {Promise<any>} */
const native = (action, profile) => new Promise((done, fail) => {
  const id = ++next
  const timer = setTimeout(() => { shell.off('message', receive); fail(new Error(`Fixture timeout: ${action}`)) }, 10_000)
  /** @param {any} message */
  const receive = message => { if (message.id === id) { clearTimeout(timer); shell.off('message', receive); message.error ? fail(new Error(message.error)) : done(message.value) } }
  shell.on('message', receive); shell.send({ id, action, profile })
})
/** @type {DevTools | null} */
let browser = null
/** @type {Map<string, {tools: DevTools, dock: string, game: string, home: string}>} */
const workers = new Map()
try {
  browser = await DevTools.connect(Number(port))
  const chooserBrowser = browser, chooser = await browser.page(t => t.url.includes('?view=profiles'))
  const evaluate = (/** @type {string} */ expression) => chooserBrowser.evaluate(chooser, expression)
  const state = async () => /** @type {State} */ (await evaluate('window.akagi.invoke("list_profiles")'))
  const runtime = (/** @type {State} */ s, /** @type {string} */ id) => s.clients.find(c => c.id === id)
  await until(state, s => s.profiles.length === 1, 'initial registry')
  assert.deepEqual(Object.keys(await state()).sort(), ['clients', 'defaults', 'error', 'profiles', 'revision'])
  assert.equal((await state()).profiles[0].id, 'existing')
  const created = /** @type {State} */ (await evaluate('window.akagi.invoke("create_profile", {name:"Tenhou",platform:"Tenhou"})'))
  const b = created.profiles.find(p => p.platform === 'Tenhou'); assert.ok(b)
  await evaluate('window.akagi.invoke("rename_profile", {id:"existing",name:"Main"})')
  const a = 'existing'
  const home = (/** @type {string} */ id) => id === 'existing' ? root : path.join(root, 'profiles', id)
  const marker = (/** @type {string} */ id) => path.join(home(id), 'fake-core-released')
  const open = (/** @type {string} */ id) => evaluate(`window.akagi.invoke('open_profile',{id:${JSON.stringify(id)}})`)
  async function attach(/** @type {string} */ id) {
    await until(state, s => runtime(s, id)?.state === 'active', `${id} ready`)
    const logs = path.join(home(id), 'logs')
    const file = fs.readdirSync(logs).filter(f => /^shell-.*\.log$/.test(f)).sort().at(-1); assert.ok(file)
    const endpoint = Number(fs.readFileSync(path.join(logs, file), 'utf8').match(/local DevTools port (\d+)/)?.[1]); assert.ok(endpoint)
    const tools = await DevTools.connect(endpoint)
    const dock = await tools.page(t => t.url.startsWith('app://dock/')), game = await tools.page(t => t.url === 'about:blank')
    assert.equal((await tools.call('Target.getTargets')).targetInfos.some((/** @type {Target} */ t) => /^https?:/.test(t.url)), false)
    await until(() => tools.evaluate(dock, 'window.akagi.invoke("get_monitor")'), s => s.capture.state === 'running', 'fake capture')
    assert.equal((await tools.evaluate(dock, 'window.akagi.invoke("get_monitor")')).game_ended, false)
    const client = { tools, dock, game, home: home(id) }; workers.set(id, client); return client
  }
  async function close(/** @type {string} */ id) {
    const client = workers.get(id); assert.ok(client)
    const shutting = client.tools.close()
    await until(state, s => runtime(s, id)?.state === 'closing', 'cleanup pending')
    assert.equal(fs.existsSync(marker(id)), false, 'slot stays held before the delayed core ACK')
    await assert.rejects(open(id), /正在关闭/)
    await shutting; workers.delete(id)
    await until(state, s => !runtime(s, id), 'ACK plus actual process close releases just this slot')
    assert.ok(fs.existsSync(marker(id)))
  }

  await open(a); await open(a); await open(b.id)
  let A = await attach(a); const B = await attach(b.id)
  const first = await native('inspect'), pidA = first.clients.find((/** @type {any} */ c) => c.id === a).pid, pidB = first.clients.find((/** @type {any} */ c) => c.id === b.id).pid
  assert.notEqual(pidA, pidB); assert.equal(first.clients.length, 2); assert.equal(first.visible, true)
  await open(a); const same = await native('inspect')
  assert.equal(same.clients.find((/** @type {any} */ c) => c.id === a).pid, pidA)
  assert.equal(same.clients.find((/** @type {any} */ c) => c.id === b.id).pid, pidB)
  assert.equal(same.visible, true); assert.equal(same.minimized, false)
  console.log('ok two real profile clients coexist; duplicate starting/active opens focus the same process; chooser stays open')

  const origin = 'https://washizu-account-fixture.invalid/'
  await A.tools.call('Network.setCookie', { url: origin, name: 'profile_marker', value: 'A', expires: Date.now() / 1000 + 86400 }, A.game)
  const cookies = await B.tools.call('Network.getCookies', { urls: [origin] }, B.game)
  assert.equal(cookies.cookies.some((/** @type {any} */ c) => c.name === 'profile_marker'), false)
  // A bare Xvfb has no window manager to implement iconification; observe the
  // actual native minimize request while exercising the real close event.
  const minimized = await native('close-window'); assert.equal(minimized.minimizeRequests, 1)
  await A.tools.evaluate(A.dock, 'window.akagi.invoke("return_to_profiles")')
  await until(() => native('inspect'), s => s.visible && !s.minimized, 'account button restores shared chooser during a live game')
  assert.equal(fs.existsSync(marker(a)), false); assert.equal(fs.existsSync(marker(b.id)), false)
  await native('quit')
  assert.equal((await native('inspect')).clients.length, 2, 'chooser Quit cannot terminate active clients')
  console.log('ok chooser close requests native minimize; account button works mid-game; chooser Quit leaves both clients running')

  await close(a)
  assert.equal((await state()).clients.find(c => c.id === b.id)?.state, 'active')
  assert.equal((await native('inspect')).clients.find((/** @type {any} */ c) => c.id === b.id).pid, pidB)
  assert.equal((await B.tools.evaluate(B.dock, 'window.akagi.invoke("get_monitor")')).capture.state, 'running')
  fs.unlinkSync(marker(a)); await open(a); A = await attach(a)
  const saved = await A.tools.call('Network.getCookies', { urls: [origin] }, A.game)
  assert.equal(saved.cookies.find((/** @type {any} */ c) => c.name === 'profile_marker')?.value, 'A')
  console.log('ok closing A waits for cleanup ACK and process exit; B keeps its process/capture; A can reopen with its own persistent cookies')

  await native('crash-client', a); A.tools.socket.close(); workers.delete(a)
  await until(state, s => runtime(s, a)?.state === 'failed', 'A crash is scoped to A')
  await assert.rejects(open(a), /清理未确认/)
  assert.equal((await state()).error, '')
  assert.equal((await native('inspect')).clients.find((/** @type {any} */ c) => c.id === b.id).pid, pidB)
  const extra = /** @type {State} */ (await evaluate('window.akagi.invoke("create_profile", {name:"Third",platform:"Majsoul"})'))
  const c = extra.profiles.find(p => p.name === 'Third'); assert.ok(c)
  await open(c.id); await attach(c.id)
  await until(() => Promise.resolve(fs.existsSync(marker(a))), Boolean, 'crashed fixture core is reaped independently')
  await close(c.id); await close(b.id)
  assert.equal((await state()).clients.find(c => c.id === a)?.cleanup, 'unconfirmed')
  console.log('ok failed A stays locked while B continues and a third real client opens/closes independently')

  assert.equal(fs.readFileSync(path.join(root, 'config.toml'), 'utf8'), original)
  assert.equal(fs.readFileSync(path.join(root, 'history', 'keep.jsonl'), 'utf8'), 'existing game records\n')
  for (const [width, height] of [[560, 304], [560, 368], [640, 368], [640, 432]]) {
    await chooserBrowser.call('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false }, chooser)
    for (const lang of ['zh-CN', 'zh-TW', 'en', 'ja']) {
      await evaluate(`document.querySelector('.accounts-tools button').click()`)
      await until(() => evaluate(`!!document.querySelector('.accounts-settings .mselect')`), Boolean, 'chooser application settings')
      await evaluate(`{const s=document.querySelector('.accounts-settings .mselect');Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(s,${JSON.stringify(lang)});s.dispatchEvent(new Event('change',{bubbles:true}));}`)
      await until(() => evaluate('document.querySelector("main").dataset.lang'), value => value === lang, 'chooser language')
      for (const index of [1, 2]) {
        await evaluate(`document.querySelectorAll('.accounts-settings .mset-groups button')[${index}].click()`)
        await until(() => evaluate(`document.querySelectorAll('.accounts-settings .mset-groups button')[${index}].getAttribute('aria-pressed')`), value => value === 'true', 'platform defaults')
        assert.equal(await evaluate(`document.querySelectorAll('.accounts-settings .mset-row').length`), 6)
        assert.equal(await evaluate(`document.documentElement.scrollWidth>innerWidth || Array.from(document.querySelectorAll('.accounts-settings button')).some(b=>b.scrollWidth>b.clientWidth)`), false)
      }
      await evaluate(`document.querySelector('.accounts-tools button').click()`)
      await until(() => evaluate(`!!document.querySelector('.accounts-panel')`), Boolean, 'account list restored')
      assert.equal(await evaluate(`document.documentElement.scrollWidth>innerWidth || document.querySelector('.accounts-panel').scrollWidth>document.querySelector('.accounts-panel').clientWidth`), false)
      assert.equal(await evaluate(`document.querySelector('tbody tr').getBoundingClientRect().height`), 64)
      if (height === 304) assert.equal(await evaluate(`{const panel=document.querySelector('.accounts-panel');panel.scrollHeight>panel.clientHeight}`), true, 'the short viewport exercises a scrolling account list')
      assert.equal(await evaluate(`Array.from(document.querySelectorAll('.accounts-row-tools button')).some(b=>b.scrollWidth>b.clientWidth)`), false)
      const alignment = await evaluate(`{
        const tracks=selector=>[...document.querySelectorAll(selector)].map(b=>{const r=b.getBoundingClientRect();return {x:r.x,width:r.width};});
        ({top:tracks('.accounts-tools button'),rows:tracks('.accounts-row-tools button')});
      }`)
      assert.ok(alignment.rows.every((/** @type {{x:number,width:number}} */ r, /** @type {number} */ i) => {
        const top = alignment.top[i % 2]; return Math.abs(r.x - top.x) < 0.5 && Math.abs(r.width - top.width) < 0.5
      }), `toolbar and account tracks at ${width}x${height}/${lang}: ${JSON.stringify(alignment)}`)
    }
  }
  console.log('ok concurrent/failed state rows retain compact layout across four languages; original registry data paths and files are unchanged')

  fs.unlinkSync(marker(c.id)); await open(c.id)
  const survivor = await attach(c.id)
  const journalDir = path.join(home(c.id), 'logs')
  shell.kill('SIGKILL')
  browser.socket.close(); browser = null
  await until(() => Promise.resolve(fs.readdirSync(journalDir).filter(f => /^shell-.*\.log$/.test(f)).some(f => fs.readFileSync(path.join(journalDir, f), 'utf8').includes('account chooser disconnected'))), Boolean, 'survivor observes chooser IPC loss')
  assert.equal((await survivor.tools.evaluate(survivor.dock, 'window.akagi.invoke("get_monitor")')).capture.state, 'running')
  assert.match(await survivor.tools.evaluate(survivor.dock, 'window.akagi.invoke("return_to_profiles").then(()=>"unexpected success", error=>String(error))'), /选择器连接已断开/)
  assert.equal(fs.existsSync(marker(c.id)), false)
  await survivor.tools.close(); workers.delete(c.id)
  await until(() => Promise.resolve(fs.existsSync(marker(c.id))), Boolean, 'survivor cleans up after chooser IPC loss')
  console.log('ok unexpected chooser exit leaves its client/capture running; disconnected account button reports failure; client still closes and cleans up independently')
} catch (error) { console.error(error); console.error(diagnostic.slice(-5000)); process.exitCode = 1 }
finally {
  for (const client of workers.values()) await client.tools.close().catch(() => {})
  if (browser) await browser.close().catch(() => {})
  shell.kill(); await sleep(1600)
  assert.equal(path.dirname(path.resolve(work)), path.resolve(os.tmpdir()))
  assert.ok(path.basename(work).startsWith('washizu-profiles-smoke-'))
  fs.rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 })
}
