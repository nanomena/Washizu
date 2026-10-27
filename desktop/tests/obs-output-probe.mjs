// @ts-nocheck
// Runs the actual account module with synthetic Electron bindings. No GUI,
// native sender, core, network listener or model process is started.
import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import vm from 'node:vm'
import { fileURLToPath } from 'node:url'
import * as relay from '../src/relay.js'
import * as layout from '../src/layout.js'
import * as input from '../src/input.js'
import * as preferences from '../src/view-preferences.js'

const home = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-obs-test-'))
const sender = path.join(home, 'sender.exe')
const unavailable = process.env.OBS_TEST_UNAVAILABLE === '1'
if (!unavailable) fs.writeFileSync(sender, '')
const legacy = '{"game_view":"window","spout":true}'
fs.writeFileSync(path.join(home, 'view.json'), legacy)
const timers = new Map()
let timerId = 0
const windows = [], views = [], senders = [], errors = [], calls = []
const presentation = [], messages = []
const optionEvents = []
const monitorOptionsOnly = process.env.MONITOR_OPTIONS_ONLY === '1'
const savedMonitor = { pointer_lean: false, quick_confirm: false, auto_win: false, auto_continue: false, pointer_trail: false }
let rejectMonitorSave = false
const handlers = new Map()
let start
const screen = Object.assign(new EventEmitter(), {
  getAllDisplays: () => [{ workArea: { x: 0, y: 0, width: 1920, height: 1080 } }],
  getPrimaryDisplay: () => ({ workAreaSize: { width: 1920, height: 1080 } }),
})
class Page extends EventEmitter {
  session = { setPermissionRequestHandler() {}, setPermissionCheckHandler() {} }
  debugger = { attach() {}, detach() {}, sendCommand: async () => ({ targetInfo: { targetId: 'offscreen-game' } }) }
  mainFrame = {}
  reloads = 0
  destroyed = false
  setWindowOpenHandler() {}
  setFrameRate() {}
  send(channel, event, value) {
    if (channel === 'akagi:event' && event === 'monitor-options') optionEvents.push(structuredClone(value))
  }
  loadURL() { return Promise.resolve() }
  isDestroyed() { return this.destroyed }
  close() { this.destroyed = true; this.emit('destroyed') }
  reload() { this.reloads++ }
}
class Window extends EventEmitter {
  constructor(options) { super(); this.options = options; windows.push(this) }
  contentView = { addChildView() {}, removeChildView() {} }
  minimized = false
  visible = false
  focused = false
  getContentSize() { return [1000, 750] }
  getContentBounds() { return { x: 0, y: 0, width: 1000, height: 750 } }
  setBounds() {}
  setContentSize(width, height) { this.contentSize = [width, height] }
  isMinimized() { return this.minimized }
  restore() { presentation.push('restore'); this.minimized = false }
  show() { presentation.push('show'); this.visible = true }
  focus() { assert.equal(this.visible, true); presentation.push('focus'); this.focused = true }
}
class Browser extends Window {
  webContents = new Page()
  destroy() { this.webContents.close(); this.emit('closed') }
}
class View {
  webContents = new Page()
  constructor() { views.push(this) }
  setBackgroundColor() {}
  setBounds() {}
}
class Core extends EventEmitter {
  ready = Promise.resolve({ platform: 'Majsoul', monitor: {}, version: 'test' })
  call(method, params) {
    calls.push({ method, params })
    if (monitorOptionsOnly) {
      const key = { set_monitor_lean: 'pointer_lean', set_monitor_confirm: 'quick_confirm', set_monitor_auto_win: 'auto_win', set_monitor_continue: 'auto_continue', set_monitor_trail: 'pointer_trail' }[method]
      if (key) {
        if (rejectMonitorSave) return Promise.reject(new Error('Monitor save rejected'))
        savedMonitor[key] = params.enabled
        return Promise.resolve({ ...savedMonitor })
      }
    }
    return Promise.resolve(null)
  }
}
const switches = { data: home, profile: 'existing', spout: sender, 'cdp-port': '33333', run: 'test' }
const app = Object.assign(new EventEmitter(), {
  commandLine: { getSwitchValue: name => switches[name] ?? '', appendSwitch() {} },
  isPackaged: false, userAgentFallback: 'Chromium', getName: () => 'Washizu', getVersion: () => 'test',
  setPath() {}, requestSingleInstanceLock: () => true,
  whenReady: () => ({ then(callback) { start = callback; return Promise.resolve() } }),
})
const namespaces = {
  electron: { app, screen, BaseWindow: Window, BrowserWindow: Browser, WebContentsView: View,
    Menu: { setApplicationMenu() {} }, clipboard: {}, sharedTexture: {},
    dialog: { showErrorBox: (...args) => errors.push(args) },
    ipcMain: { on() {}, handle: (name, callback) => handlers.set(name, callback) } },
  'node:child_process': { spawn: (...args) => {
    const child = Object.assign(new EventEmitter(), { stdin: new PassThrough(), stdout: new PassThrough(), stderr: new PassThrough() })
    senders.push({ args, child }); return child
  } },
  './core.js': { Core }, './input.js': input,
  './journal.js': { Journal: class { note() {} }, chromiumSwitches() {}, hasChromiumLog: () => true, prune() {}, runName: () => 'test' },
  './layout.js': layout, './dock.js': { registerDockScheme() {}, serveDock() {} }, './relay.js': relay,
  './watch.js': { COLLECT_AFTER_MS: 1000, FrameMeter: class { paint() {} }, GL_STATE: '', GlWatch: class {},
    HUNG_MS: 1000, RELOAD_QUIET_MS: 1000, WATCH_MS: 1000, WEBGL_CONTEXTS: 16, processLine() {}, worthKeeping() {} },
  './workers.js': { keepWorkersRunning: async () => {} },
  './window-frame.js': { contentSize: () => ({ width: 1000, height: 750 }), holdRatio() {} },
  './profiles.js': { ProfileStore: class { load() { return this } get() { return { id: 'existing', name: 'Original', platform: 'Majsoul' } } home() { return home } },
    PLATFORMS: { Majsoul: { name: 'Mahjong Soul' } }, homePlatform: () => 'Majsoul' },
  './view-preferences.js': preferences,
}
const accountProcess = Object.assign(new EventEmitter(), {
  platform: 'win32', env: {}, argv: [], pid: 1234, connected: true, versions: {},
  send(message, callback) { messages.push(message); presentation.push(message.type); callback?.(null) },
})
const context = vm.createContext({ console, URL, Buffer,
  process: accountProcess,
  setTimeout(callback, delay) { const id = ++timerId; timers.set(id, { callback, delay }); return id },
  clearTimeout: id => timers.delete(id), setInterval: () => ++timerId, clearInterval() {},
})
async function probe() {
try {
  const filename = fileURLToPath(new URL('../src/game-main.js', import.meta.url))
  const module = new vm.SourceTextModule(fs.readFileSync(filename, 'utf8'), {
    context, identifier: filename, initializeImportMeta: meta => { meta.url = new URL('../src/game-main.js', import.meta.url).href },
  })
  await module.link(async specifier => {
    const namespace = namespaces[specifier] ?? await import(specifier)
    return new vm.SyntheticModule(Object.keys(namespace), function () {
      for (const [key, value] of Object.entries(namespace)) this.setExport(key, value)
    }, { context })
  })
  await module.evaluate()
  accountProcess.emit('message', { type: 'focus' })
  assert.deepEqual(presentation, [], 'a focus request before window creation adds no retry or state')
  await start()
  const account = windows[0]
  assert.equal(account.visible, false)
  assert.deepEqual(messages, [])
  views[0].webContents.emit('did-finish-load')
  assert.deepEqual(presentation, ['show', 'focus', 'profile-ready'], 'the first Open presents and focuses before acknowledging readiness')
  assert.equal(account.visible, true)
  assert.equal(account.focused, true)
  views[0].webContents.emit('did-finish-load')
  assert.equal(messages.length, 1, 'a page reload never repeats the initial window presentation')
  presentation.length = 0
  account.visible = account.focused = false
  account.minimized = true
  accountProcess.emit('message', { type: 'focus' })
  assert.deepEqual(presentation, ['restore', 'show', 'focus'])
  assert.equal(account.minimized, false)
  presentation.length = 0
  account.visible = account.focused = false
  app.emit('second-instance')
  assert.deepEqual(presentation, ['show', 'focus'], 'a hidden account is shown as well as focused')
  assert.equal(windows.length, 1, 'focus never creates a second account window')
  if (process.env.WINDOW_PRESENTATION_ONLY === '1') {
    console.log('First Open, IPC focus and second-instance presentation lifecycle: passed')
    return
  }
  const invoke = (command, args = {}) => handlers.get('akagi:invoke')({ sender: views[0].webContents }, command, args)
  const options = async () => (await invoke('get_monitor_options')).value
  if (monitorOptionsOnly) {
    // A caller other than React's own onClick must update the mounted view,
    // not just the cached get_monitor_options response.
    for (const [command, key] of [['set_monitor_continue', 'auto_continue'], ['set_monitor_lean', 'pointer_lean'], ['set_monitor_confirm', 'quick_confirm'], ['set_monitor_auto_win', 'auto_win'], ['set_monitor_trail', 'pointer_trail']]) {
      for (const enabled of [true, false]) {
        optionEvents.length = 0
        const reply = await invoke(command, { enabled })
        assert.equal(reply.ok, true)
        assert.equal(optionEvents.length, 1, `${command} must publish exactly once after persistence`)
        assert.equal(optionEvents[0][key], enabled)
        assert.deepEqual(optionEvents[0], structuredClone(await options()))
        assert.deepEqual(structuredClone(reply.value), optionEvents[0])
      }
    }
    const before = await options()
    optionEvents.length = 0
    rejectMonitorSave = true
    const rejected = await invoke('set_monitor_continue', { enabled: true })
    assert.equal(rejected.ok, false)
    assert.match(rejected.error, /Monitor save rejected/)
    assert.equal(optionEvents.length, 0, 'a failed save must not publish a successful setting')
    assert.deepEqual(await options(), before)
    assert.equal(calls.some(call => call.method === 'open_game'), false)
    console.log('Persisted monitor options broadcast and failed-save isolation: passed')
    return
  }
  assert.equal((await options()).obs_name, 'Washizu')
  assert.equal((await options()).obs_output, true)
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(home, 'obs.json'), 'utf8')), { enabled: true })
  assert.equal(fs.readFileSync(path.join(home, 'view.json'), 'utf8'), legacy)
  assert.equal((await invoke('open_monitor_game', { bounds: { x: 0, y: 0, width: 880, height: 495, visible: true } })).ok, true)
  const game = windows.find(window => window.options.webPreferences?.offscreen)
  assert.equal(views.length, 1, 'only the dock uses a WebContentsView')
  assert.equal(game.options.width, 1280)
  assert.equal(game.options.height, 720)
  assert.deepEqual(game.contentSize, [1280, 720], 'the game content is sized before its first load')
  game.contentSize = [1282, 722]
  screen.emit('display-metrics-changed')
  assert.deepEqual(game.contentSize, [1280, 720], 'display changes restore the fixed content area')
  assert.equal(game.options.webPreferences.offscreen.deviceScaleFactor, 2)
  assert.equal(calls.at(-1).method, 'open_game')
  if (unavailable) {
    assert.equal((await options()).obs_available, false)
    assert.equal((await options()).obs_state, 'failed')
    assert.match((await invoke('set_obs_output', { enabled: true })).error, /Windows Spout2/)
    assert.equal(senders.length, 0)
    assert.equal(game.webContents.isDestroyed(), false)
    assert.equal((await invoke('set_obs_output', { enabled: false })).value.obs_state, 'off')
    console.log('Unavailable OBS sender: passed')
  } else {
    assert.deepEqual(senders[0].args[1], ['1234', '--name', 'Washizu'])
    senders[0].child.stderr.write('sending as Washizu\n')
    assert.equal((await options()).obs_state, 'sending')
    assert.equal((await invoke('set_obs_output', { enabled: false })).value.obs_state, 'off')
    assert.equal(game.webContents.isDestroyed(), false)
    assert.equal(senders[0].child.stdin.writableEnded, true)
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(home, 'obs.json'), 'utf8')), { enabled: false })
    assert.equal((await invoke('set_obs_output', { enabled: 'yes' })).ok, false)
    assert.equal((await invoke('set_obs_output', { enabled: true })).ok, true)
    for (const command of ['set_monitor_game_view', 'set_monitor_spout', 'set_monitor_stream', 'copy_stream_url']) {
      assert.equal((await invoke(command)).error, `Unknown command: ${command}`)
    }
    assert.equal(Object.keys(await options()).some(key => /^(stream_|spout_|game_view)/.test(key)), false)
    const beforeWindows = windows.length
    app.emit('child-process-gone', {}, { type: 'GPU', reason: 'crashed' })
    app.emit('child-process-gone', {}, { type: 'GPU', reason: 'crashed' })
    assert.equal(errors.length, 1)
    const reload = [...timers.values()].find(timer => timer.delay === 1000)
    assert.equal([...timers.values()].filter(timer => timer.delay === 1000).length, 1)
    assert.ok(reload)
    reload.callback()
    assert.equal(game.webContents.reloads, 1)
    assert.equal(windows.length, beforeWindows, 'GPU recovery never creates an embedded game window')
    assert.equal((await options()).obs_output, true)
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(home, 'obs.json'), 'utf8')), { enabled: true })
    assert.equal(fs.readFileSync(path.join(home, 'view.json'), 'utf8'), legacy)
    await invoke('set_obs_output', { enabled: false })
    console.log('OBS contract, fixed offscreen rendering, GPU recovery and migration: passed')
  }
} finally {
  const temporaryRoot = path.resolve(os.tmpdir()) + path.sep
  if (!path.resolve(home).startsWith(temporaryRoot)) throw new Error('Unexpected test directory')
  fs.rmSync(home, { recursive: true, force: true })
}
}
await probe()
