// @ts-check
// The shell, for real: Electron, the built interface and a stand-in core, looked
// at from outside over the shell's own DevTools port. It is here for what the
// unit tests cannot see — whether the page, served the way the shell serves it,
// still draws. A content policy that is one directive short breaks nothing
// loudly: the tiles just turn into text.
//
//   node tests/smoke.mjs [--dock=<dir>]      (default: ../frontend/dist)
//
// Needs a display; on a machine without one, `xvfb-run -a node tests/smoke.mjs`.
// Linux and macOS only: the stand-in core is started through a shell script.

import { spawn } from 'node:child_process'
import fs from 'node:fs'
import { createRequire } from 'node:module'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = path.dirname(fileURLToPath(import.meta.url))
const DESKTOP = path.dirname(HERE)
const electron = /** @type {string} */ (/** @type {unknown} */ (createRequire(import.meta.url)('electron')))
const dock = path.resolve(process.argv.find(arg => arg.startsWith('--dock='))?.slice(7) ?? path.join(DESKTOP, '..', 'frontend', 'dist'))
if (!fs.existsSync(path.join(dock, 'index.html'))) throw new Error(`No built interface at ${dock}: run "npm run build" in frontend/ first.`)

const sleep = (/** @type {number} */ ms) => new Promise(done => setTimeout(done, ms))
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'akagi-smoke-'))
fs.mkdirSync(path.join(work, 'data'))
fs.writeFileSync(path.join(work, 'data', 'config.toml'), '[platform]\nkind = "Majsoul"\n')
const core = path.join(work, 'core.sh')
fs.writeFileSync(core, `#!/bin/sh\nexec "${process.execPath}" "${path.join(HERE, 'stand-in-core.js')}" --mode monitor "$@"\n`, { mode: 0o755 })
const port = await new Promise(done => {
  const probe = net.createServer().listen(0, '127.0.0.1', () => { const { port } = /** @type {net.AddressInfo} */ (probe.address()); probe.close(() => done(port)) })
})

// On a screen of scale 1.5, as many are: CSS and screen pixels differ, which is
// where a page drawn for the stream could be laid out apart from its view.
const SCREEN_SCALE = 1.5
// `--run` marks the start as one that has its logging switches already (see `src/journal.js`): without
// it the shell starts itself again with them, and the process watched here would be gone.
const shell = spawn(electron, [DESKTOP, '--no-sandbox', `--force-device-scale-factor=${SCREEN_SCALE}`, `--data=${path.join(work, 'data')}`, '--profile=existing', `--core=${core}`, `--dock=${dock}`, `--cdp-port=${port}`, '--run=smoke'], { stdio: ['ignore', 'pipe', 'pipe'] })
let said = ''
shell.stdout.on('data', text => { said += text })
shell.stderr.on('data', text => { said += text })
/** @type {number | string | null} */
let exited = null
shell.on('exit', (code, signal) => { exited = signal ?? code })

/** @type {string[]} */
const failures = []
const check = (/** @type {string} */ name, /** @type {unknown} */ ok, detail = '') => {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`)
  if (!ok) failures.push(name)
}

try {
  /** @type {any} */
  let version
  for (let i = 0; i < 150 && exited === null && !version; i++) {
    version = await fetch(`http://127.0.0.1:${port}/json/version`).then(reply => reply.json(), () => null)
    if (!version) await sleep(200)
  }
  if (!version) throw new Error(`The shell did not open its DevTools port (exit: ${exited}).\n${said.slice(-2000)}`)

  const socket = new WebSocket(version.webSocketDebuggerUrl)
  await new Promise((done, fail) => { socket.onopen = done; socket.onerror = () => fail(new Error('DevTools socket')) })
  let next = 0
  /** @type {Map<number, { done: (value: any) => void, fail: (reason: Error) => void }>} */
  const waiting = new Map()
  /** @type {any[]} */
  const events = []
  socket.onmessage = message => {
    const data = JSON.parse(String(message.data))
    const asked = waiting.get(data.id)
    if (asked) {
      waiting.delete(data.id)
      if (data.error) asked.fail(new Error(JSON.stringify(data.error)))
      else asked.done(data.result)
    } else if (data.method) events.push(data)
  }
  const send = (/** @type {string} */ method, params = {}, /** @type {string | undefined} */ sessionId = undefined) => new Promise((done, fail) => {
    const id = ++next
    waiting.set(id, { done, fail })
    socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }))
  })
  const pages = async () => (await send('Target.getTargets')).targetInfos.filter((/** @type {any} */ t) => t.type === 'page')

  /** @type {any} */
  let page
  for (let i = 0; i < 100 && !page; i++) {
    page = (await pages()).find((/** @type {any} */ t) => t.url.startsWith('app://dock/'))
    if (!page) await sleep(200)
  }
  check('the interface is served from app://dock/', page)
  if (!page) throw new Error(`No interface page (exit: ${exited}).\n${said.slice(-2000)}`)
  const { sessionId } = await send('Target.attachToTarget', { targetId: page.targetId, flatten: true })
  await send('Runtime.enable', {}, sessionId)
  await send('Log.enable', {}, sessionId)
  const evaluate = async (/** @type {string} */ expression) => {
    const reply = await send('Runtime.evaluate', { expression: `(async()=>(${expression}))()`, awaitPromise: true, returnByValue: true }, sessionId)
    if (reply.exceptionDetails) throw new Error(reply.exceptionDetails.exception?.description ?? reply.exceptionDetails.text)
    return reply.result.value
  }

  // The page draws itself from the core's snapshot, and opens the game.
  /** @type {any} */
  let seen = {}
  for (let i = 0; i < 75; i++) {
    seen = await evaluate(`(async () => ({
      capture: (await window.akagi.invoke('get_monitor')).capture.state,
      drawn: document.querySelectorAll('.mt img').length,
      named: [...document.querySelectorAll('.mt .mt-ghost')].map(tile => tile.textContent).filter(Boolean),
      stage: (r => r && [Math.round(r.width), Math.round(r.height)])(document.querySelector('.mon-stage')?.getBoundingClientRect()),
      refused: await window.akagi.invoke('no_such_command').catch(error => String(error)),
    }))()`).catch(error => ({ error: String(error) }))
    if (seen.capture === 'running' && seen.drawn > 0 && seen.named?.length === 0) break
    await sleep(200)
  }
  check('the page reaches the core through the shell', !seen.error, seen.error)
  check('tiles are drawn as tiles', seen.drawn > 0 && seen.named?.length === 0, `${seen.drawn} drawn, ${seen.named?.length} left as text ${JSON.stringify(seen.named ?? [])}`)
  check('the page had the game opened, and the core was told', seen.capture === 'running', seen.capture)
  check('a command the shell does not know is refused by name', seen.refused === 'Unknown command: no_such_command', seen.refused)

  const game = (await pages()).find((/** @type {any} */ t) => t.targetId !== page.targetId)
  check('the game view is a page of its own, in a session of its own', game && game.browserContextId !== page.browserContextId)
  if (game) {
    const attached = await send('Target.attachToTarget', { targetId: game.targetId, flatten: true })
    const size = (await send('Runtime.evaluate', { expression: '[innerWidth, innerHeight]', returnByValue: true }, attached.sessionId)).result.value
    check('the game uses fixed offscreen CSS dimensions', size[0] === 1280 && size[1] === 720, size.join('×'))
    const scale = (await send('Runtime.evaluate', { expression: 'devicePixelRatio', returnByValue: true }, attached.sessionId)).result.value
    check('the offscreen game draws at scale two independently of the desktop', scale === 2, String(scale))
  }

  const before = await evaluate(`window.akagi.invoke('get_monitor_options')`)
  check('OBS output exposes its name and availability without an HTTP address',
    typeof before.obs_available === 'boolean' && typeof before.obs_name === 'string' && !Object.keys(before).some(key => /^(stream_|game_view|spout_)/.test(key)))
  const off = await evaluate(`window.akagi.invoke('set_obs_output', { enabled: false })`)
  check('the single OBS switch turns the sender off', off.obs_output === false && off.obs_state === 'off')
  check('the output preference is a single boolean', JSON.stringify(JSON.parse(fs.readFileSync(path.join(work, 'data', 'obs.json'), 'utf8'))) === '{"enabled":false}')
  const still = await evaluate(`(await window.akagi.invoke('get_monitor')).capture.state`)
  check('turning OBS output off leaves the game and capture running', still === 'running')
  if (!before.obs_available) {
    const refused = await evaluate(`window.akagi.invoke('set_obs_output', { enabled: true }).then(() => '', error => String(error))`)
    check('unsupported OBS output fails explicitly', refused.includes('Windows Spout2'))
  }
  for (const command of ['set_monitor_game_view', 'set_monitor_spout', 'set_monitor_stream', 'copy_stream_url']) {
    const refused = await evaluate(`window.akagi.invoke('${command}', {}).then(() => '', error => String(error))`)
    check(`removed command ${command} is rejected`, refused === `Unknown command: ${command}`)
  }

  const problems = events.filter(event => event.sessionId === sessionId)
    .map(event => event.method === 'Runtime.exceptionThrown' ? String(event.params.exceptionDetails.exception?.description ?? event.params.exceptionDetails.text)
      : event.method === 'Log.entryAdded' && event.params.entry.level === 'error' ? String(event.params.entry.text) : '')
    .filter(Boolean)
  check('the page logged no errors', problems.length === 0, problems.map(text => text.slice(0, 200)).join(' | '))
  socket.close()
} catch (error) {
  check('the smoke test ran to its end', false, String(error instanceof Error ? error.stack : error))
} finally {
  shell.kill()
  for (let i = 0; i < 50 && exited === null; i++) await sleep(100)
  if (exited === null) shell.kill('SIGKILL')
  fs.rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 })
}
process.exit(failures.length ? 1 : 0)
