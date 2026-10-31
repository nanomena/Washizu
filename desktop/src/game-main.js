// @ts-check
// The window one Washizu account runs in. One Chromium on every platform shows the game and
// the monitor's dock under it, in one 4:3 window, and serves the DevTools
// endpoint the core captures the game through. Everything else — the bridge,
// the tracker, the bot, autoplay, the monitor snapshot — is the core's
// (`src/headless`), a child process this file starts and talks to in JSON lines.
//
//   --data=<dir>      the account library root (resolved by main.js)
//   --profile=<id>    the stable account id selected by the chooser
//   --core=<file>     the core's executable (default: the packaged one)
//   --dock=<dir>      the built interface (default: the packaged one)
//   --cdp-port=<n>    the local DevTools port (default: any free port)
//   --spout=<file>    the Spout sender (Windows; default: the packaged one)
// Each can also be given as AKAGI_DATA, AKAGI_CORE, AKAGI_DOCK, AKAGI_CDP_PORT, AKAGI_SPOUT.
//
// The game is always drawn offscreen at fixed 1280 × 720 CSS pixels and scale 2.
// GPU textures go to the dock and, while OBS output is enabled, the Windows
// Spout sender. Window size and display scaling never change the game's canvas.

import { BaseWindow, BrowserWindow, Menu, WebContentsView, app, clipboard, dialog, ipcMain, screen, sharedTexture } from 'electron'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import { createServer } from 'node:net'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { Core } from './core.js'
import { TEXT_MAX, toGame } from './input.js'
import { Journal, chromiumSwitches, hasChromiumLog, prune, runName } from './journal.js'
import { PhysicalPointerObserver, nativeWindowIdentity } from './physical-pointer.js'
import { MIN, conform, gameBounds, manualPointerArea, onView, openingSize, restoredBounds, trailBounds } from './layout.js'
import { registerDockScheme, serveDock } from './dock.js'
import { Relay, SpoutTaker, pageTaker } from './relay.js'
import { COLLECT_AFTER_MS, FrameMeter, GL_STATE, GlWatch, HUNG_MS, RELOAD_QUIET_MS, WATCH_MS, WEBGL_CONTEXTS, processLine, worthKeeping } from './watch.js'
import { keepWorkersRunning } from './workers.js'
import { contentSize, holdRatio, presentWindow } from './window-frame.js'
import { ProfileStore, PLATFORMS, homePlatform } from './profiles.js'
import { readObsPreference, saveObsPreference, spoutSenderName } from './view-preferences.js'
import { applyGameProxy } from './profile-proxy.js'

const HERE = path.dirname(fileURLToPath(import.meta.url))
/** The interface's address. A scheme of its own, so the page has an origin and absolute paths. */
const DOCK = 'app://dock/'
/** The game's session: its own cookies and storage, kept between runs, apart from the interface's. */
const GAME_PARTITION = 'persist:game'
/** The page's own background: behind the interface while it loads, and beside the canvas if the window is ever off its ratio. */
const BACKGROUND = '#ece9e4'
/** What the game may ask for. It copies room numbers and pastes them; nothing else is its to have. */
const GAME_PERMISSIONS = new Set(['clipboard-read', 'clipboard-sanitized-write'])
/** @param {string} name */
const option = name => app.commandLine.getSwitchValue(name) || process.env[`AKAGI_${name.toUpperCase().replaceAll('-', '_')}`] || ''
const CORE_NAME = process.platform === 'win32' ? 'akagi-core.exe' : 'akagi-core'

const profileStore = new ProfileStore(path.resolve(option('data'))).load()
const PROFILE = profileStore.get(option('profile'))
const DATA = profileStore.home(PROFILE.id)
if (homePlatform(DATA) !== PROFILE.platform) throw new Error('档案的平台与配置不一致，资料未作更改。')
const CORE = path.resolve(option('core') || (app.isPackaged ? path.join(process.resourcesPath, 'core', CORE_NAME) : path.join(HERE, '..', '..', 'target', 'release', CORE_NAME)))
const DOCK_FILES = path.resolve(option('dock') || (app.isPackaged ? path.join(process.resourcesPath, 'dock') : path.join(HERE, '..', '..', 'frontend', 'dist')))
const WINDOW_STATE = path.join(DATA, 'window.json')
const SPOUT = path.resolve(option('spout') || (app.isPackaged ? path.join(process.resourcesPath, 'spout', 'spout-sender.exe') : path.join(HERE, '..', 'native', 'spout', 'build', 'spout-sender.exe')))
/** The name a streaming program finds the game's frames under. */
const SPOUT_NAME = spoutSenderName(PROFILE)
/**
 * The game rendered offscreen: its page's size in CSS pixels, how many pixels
 * each is drawn with, and how many frames a second. Mahjong Soul draws its
 * canvas at 2560 × 1440, so at twice 1280 × 720 every one of its pixels is a
 * pixel of the frame; a streaming program scales that down to its output.
 */
const OFFSCREEN = Object.freeze({ width: 1280, height: 720, scale: 2, fps: 60 })
/** Where the shell's record and Chromium's go, beside the core's run folders; and this start's name for its files. */
const LOGS = path.join(DATA, 'logs')
const RUN = option('run') || runName()
const journal = new Journal(path.join(LOGS, `shell-${RUN}.log`))
/** @param {string} message */
const note = message => journal.note(message)

/** @returns {Promise<number>} a local port nothing is listening on */
const freePort = () => new Promise((done, fail) => {
  const probe = createServer().once('error', fail).listen(0, '127.0.0.1', () => {
    const { port } = /** @type {import('node:net').AddressInfo} */ (probe.address())
    probe.close(() => done(port))
  })
})

/** @type {BaseWindow | null} */
let window = null
/** @type {WebContentsView | null} */
let dock = null
/** The game's page drawn offscreen, in a window that is never shown. */
/** @type {BrowserWindow | null} */
let offscreenGame = null
/** @type {Relay | null} */
let relay = null
/** @type {SpoutTaker | null} */
let spout = null
/** A pending reload after a GPU process loss; repeated notifications share it. */
/** @type {NodeJS.Timeout | undefined} */
let gpuReload
/** @type {Core | null} */
let core = null
/** Autoplay's pointer, drawn over the game while the option is on. */
/** @type {BrowserWindow | null} */
let trail = null
const physicalPointer = new PhysicalPointerObserver(params => running().call('set_user_pointer', params), error => note(`physical pointer ownership unavailable: ${error}`))
/** Where the game view is in the window's content, while it is shown. */
/** @type {import('./layout.js').Rect | null} */
let gameAt = null
/**
 * The core's monitor preferences and this shell's single OBS output contract.
 * @typedef {{ pointer_trail?: boolean, pointer_lean?: boolean,
 *   quick_confirm?: boolean, auto_win?: boolean, auto_continue?: boolean,
 *   version?: string, obs_output?: boolean, obs_available?: boolean,
 *   obs_state?: 'off' | 'starting' | 'sending' | 'failed' | 'stopped',
 *   obs_name?: string, platform?: 'Majsoul' | 'Tenhou', profile_name?: string }} Options
 */
/** @type {Options} */
let options = {}
/** The account's only output preference; the renderer always remains offscreen. */
let obsEnabled = readObsPreference(DATA)
/** Whether a Windows Spout2 sender is installed. */
const SPOUT_AVAILABLE = process.platform === 'win32' && fs.existsSync(SPOUT)
let cdpPort = 0
let quitting = false
let stoppingCore = false
let profileReleased = false

/** Stop with a message. Nothing is left half-done by it: the core goes when we do. */
function fail(/** @type {string} */ title, /** @type {string} */ detail) {
  if (quitting) return
  quitting = true
  dialog.showErrorBox(title, detail)
  app.exit(1)
}

/** The end of what the core wrote to stderr, as plain text. */
function lastWords() {
  // The core colours its console output; a dialog would show the codes.
  return (core?.stderr ?? '').replace(/\x1b\[[0-9;]*m/g, '').trim().split('\n').slice(-12).join('\n')
}

/** @returns {unknown} the JSON in `file`, or null */
function readJson(/** @type {string} */ file) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')) } catch { return null }
}

/** The shell-owned output settings; no output address or rendering mode exists. @returns {Options} */
function obsOptions() {
  return {
    obs_output: obsEnabled, obs_available: SPOUT_AVAILABLE, obs_name: SPOUT_NAME,
    obs_state: !obsEnabled ? 'off' : !SPOUT_AVAILABLE ? 'failed' : spout?.state ?? (relay ? 'starting' : 'stopped'),
  }
}

/** Tell the interface the options again, for a change it did not ask for. */
function tellOptions() {
  options = { ...options, ...obsOptions() }
  dock?.webContents.send('akagi:event', 'monitor-options', options)
}

/** Present this account for both its first opening and later focus requests. */
function showWindow() {
  presentWindow(window)
}

/** @param {import('./core.js').Ready} ready */
function openWindow(ready) {
  if (ready.platform !== PROFILE.platform) throw new Error('core 返回的平台与账号档案不一致。')
  options = { ...ready.monitor, version: ready.version, ...obsOptions(), platform: PROFILE.platform, profile_name: PROFILE.name }
  const saved = restoredBounds(readJson(WINDOW_STATE), screen.getAllDisplays().map(display => display.workArea))
  const size = saved ?? openingSize(screen.getPrimaryDisplay().workAreaSize)
  const frame = window = new BaseWindow({
    ...(saved ? { x: saved.x, y: saved.y } : {}),
    width: size.width,
    height: size.height,
    useContentSize: true,
    minWidth: MIN.width,
    minHeight: MIN.height,
    title: `Washizu · ${PROFILE.name} · ${PLATFORMS[PROFILE.platform].name}`,
    // A maximized or full-screen window cannot keep the canvas's ratio.
    maximizable: false,
    fullscreenable: false,
    backgroundColor: BACKGROUND,
    show: false,
  })
  holdRatio(frame)

  const view = dock = new WebContentsView({ webPreferences: { preload: path.join(HERE, 'preload.cjs'), sandbox: true, contextIsolation: true } })
  view.setBackgroundColor(BACKGROUND)
  frame.contentView.addChildView(view)
  const fill = () => view.setBounds({ x: 0, y: 0, ...contentSize(frame) })
  fill()
  frame.on('resize', fill)
  // The trail is a window of its own over the game view, and has to follow it.
  frame.on('move', placeTrail)
  frame.on('resize', placeTrail)
  // The user's hand on the mouse over the game: autoplay holds its presses
  // until it rests, so that the two do not get in each other's way.
  const watch = setInterval(watchCursor, 50)
  frame.on('closed', () => clearInterval(watch))

  // The interface is one page of our own files: it goes nowhere and opens nothing.
  const page = view.webContents
  page.setWindowOpenHandler(() => ({ action: 'deny' }))
  page.on('will-navigate', (event, url) => { if (!url.startsWith(DOCK)) event.preventDefault() })
  page.on('render-process-gone', () => page.reload())
  page.once('did-finish-load', () => {
    showWindow()
    if (process.connected) process.send?.({ type: 'profile-ready' }, error => { if (error) note(`account chooser notification failed: ${error.message}`) })
  })
  page.on('did-fail-load', (_event, _code, description, _url, isMainFrame) => {
    if (isMainFrame) fail('The interface could not be loaded.', `${description}\n${DOCK_FILES}`)
  })
  void page.loadURL(`${DOCK}index.html?view=monitor`)

  frame.on('close', () => {
    const [x, y] = frame.getPosition()
    try { fs.writeFileSync(WINDOW_STATE, JSON.stringify({ x, y, ...contentSize(frame) })) } catch { /* where the window was is a convenience */ }
  })
  frame.on('closed', () => {
    clearTimeout(gpuReload)
    stopSpout()
    // Views outlive the window they were in unless they are closed.
    offscreenGame?.destroy()
    view.webContents.close()
    trail?.destroy()
    window = dock = offscreenGame = trail = null
    relay = spout = null
    app.quit()
  })
}

/** The game's offscreen page. */
function gamePage() {
  return offscreenGame?.webContents ?? null
}

/**
 * The game drawn offscreen, in a window that is never shown. The page's size
 * comes from the window's content area. Set it explicitly after construction:
 * at fractional display scales the initial native bounds may be rounded up.
 * The fixed output size is checked again on every frame.
 */
function createOffscreenGame() {
  const win = new BrowserWindow({
    x: 0, y: 0, width: OFFSCREEN.width, height: OFFSCREEN.height, show: false, frame: false, thickFrame: false, resizable: false, skipTaskbar: true,
    webPreferences: {
      partition: GAME_PARTITION, sandbox: true, contextIsolation: true, backgroundThrottling: false,
      offscreen: { useSharedTexture: true, deviceScaleFactor: OFFSCREEN.scale },
    },
  })
  const page = win.webContents
  page.setFrameRate(OFFSCREEN.fps)
  guardGame(page)
  const meter = new FrameMeter(OFFSCREEN.fps, WATCH_MS)
  watchGame(page, meter)
  const resize = () => win.setContentSize(OFFSCREEN.width, OFFSCREEN.height)
  resize()
  const fanout = relay = new Relay({ expected: { width: OFFSCREEN.width * OFFSCREEN.scale, height: OFFSCREEN.height * OFFSCREEN.scale }, onSize: resize, log: note })
  fanout.add(pageTaker({ sharedTexture, frame: () => dock?.webContents.mainFrame ?? null, log: note }))
  page.on('paint', event => {
    meter.paint()
    if (event.texture) fanout.paint(event.texture)
  })
  // The page draws its own cursor no more: the dock's stage shows the one it asks for.
  page.on('cursor-changed', (_event, type) => dock?.webContents.send('akagi:event', 'game-cursor', type))
  screen.on('display-metrics-changed', resize)
  win.on('closed', () => screen.off('display-metrics-changed', resize))
  startSpout()
  return win
}

/** The OBS switch controls the sender only; the offscreen game is independent. */
function startSpout() {
  if (!relay || !obsEnabled || !SPOUT_AVAILABLE) {
    stopSpout()
    return
  }
  if (spout) return
  spout = new SpoutTaker({ exe: SPOUT, pid: process.pid, sender: SPOUT_NAME, spawn, log: note, onState: () => tellOptions() })
  relay.add(spout)
}

function stopSpout() {
  if (!spout) return
  relay?.delete(spout)
  spout.stop()
  spout = null
}

/** What the game's offscreen page may do. @param {Electron.WebContents} page */
function guardGame(page) {
  const web = (/** @type {string} */ url) => /^https?:\/\//i.test(url)
  page.on('will-navigate', (event, url) => { if (!web(url)) event.preventDefault() })
  // Signing in through another service opens a window, and that window has to
  // share the game's session to hand the sign-in back. It is an ordinary
  // window, even when the game is drawn offscreen.
  page.setWindowOpenHandler(({ url }) => web(url)
    ? { action: 'allow', overrideBrowserWindowOptions: { width: 960, height: 720, autoHideMenuBar: true, show: true, webPreferences: { session: page.session, sandbox: true, contextIsolation: true, offscreen: false } } }
    : { action: 'deny' })
  page.on('render-process-gone', (_event, details) => {
    note(`game: its renderer went (${details.reason}, exit ${details.exitCode}); loading it again`)
    page.reload()
  })
  page.session.setPermissionRequestHandler((_contents, permission, allow) => allow(GAME_PERMISSIONS.has(permission)))
  page.session.setPermissionCheckHandler((_contents, permission) => GAME_PERMISSIONS.has(permission))
}

/**
 * Note what the game's page shows of itself without touching the game: its
 * console's warnings and errors and the changes of scene, its renderer not
 * answering, and every `WATCH_MS` whether its drawing context is still there
 * and, drawn offscreen, how many frames it painted (`meter`). Its context lost
 * notes the application's processes too, and is acted on at once: a page
 * without its context draws nothing and its game plays no more turns, so it is
 * loaded again (the game rejoins the table it was at). A page that does not
 * answer for `HUNG_MS` has its renderer ended, and is loaded again as any page
 * whose renderer went is. No one need be watching for either. See `watch.js`.
 * @param {Electron.WebContents} page @param {FrameMeter | null} meter
 */
function watchGame(page, meter) {
  const gl = new GlWatch()
  let late = false
  let reloadedAt = 0
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let hung
  page.on('console-message', details => {
    if (worthKeeping(details.level, details.message)) note(`game console ${details.level}: ${details.message.trim().slice(0, 600)}`)
  })
  page.on('unresponsive', () => {
    note('game: the page does not answer')
    clearTimeout(hung)
    hung = setTimeout(() => {
      if (page.isDestroyed()) return
      note(`game: no answer for ${HUNG_MS / 1000} s; ending its renderer to load it again`)
      page.forcefullyCrashRenderer()
    }, HUNG_MS)
  })
  page.on('responsive', () => {
    clearTimeout(hung)
    note('game: the page answers again')
  })
  page.on('did-finish-load', () => {
    gl.reset()
    note('game: page loaded')
  })
  const look = async () => {
    /** @type {Promise<'late'>} */
    const timeout = new Promise(done => setTimeout(() => done('late'), 5000))
    const state = await Promise.race([page.executeJavaScript(GL_STATE, false), timeout]).catch(() => null)
    if (state === 'late') {
      if (!late) note(`game: the page did not answer a look in 5 s`)
      late = true
      return
    }
    if (late) note('game: the page answers a look again')
    late = false
    const frames = meter?.take()
    if (frames?.line) note(frames.line)
    const line = gl.seen(state)
    if (!line) return
    note(frames ? `${line}; ${frames.frames} frames painted in ${WATCH_MS / 1000} s` : line)
    if (!state?.lost) return
    note(`processes: ${processLine(app.getAppMetrics())}`)
    if (Date.now() - reloadedAt < RELOAD_QUIET_MS) {
      note('game: lost again soon after loading it again; left as it is')
      return
    }
    reloadedAt = Date.now()
    note('game: loading the page again for its lost context')
    page.reload()
  }
  const timer = setInterval(() => { if (!page.isDestroyed()) void look() }, WATCH_MS)
  page.once('destroyed', () => clearInterval(timer))
}

/**
 * Collect the game page's garbage once a game has ended (see `COLLECT_AFTER_MS`): the WebGL contexts
 * its client leaks are let go with the canvases they were made on. Uses the page's DevTools session,
 * taken only if no one has it, and given back if it was taken here.
 */
async function collectGameGarbage() {
  const page = gamePage()
  if (!page || page.isDestroyed()) return
  const tools = page.debugger
  const taken = !tools.isAttached()
  try {
    if (taken) tools.attach('1.3')
    const started = Date.now()
    await tools.sendCommand('HeapProfiler.collectGarbage')
    note(`game: garbage collected after the game in ${Date.now() - started} ms`)
  } catch (error) {
    note(`game: garbage not collected: ${error instanceof Error ? error.message : error}`)
  } finally {
    if (taken && tools.isAttached()) tools.detach()
  }
}

/**
 * A GPU loss invalidates offscreen textures. Report it and reload the same
 * offscreen target; never change rendering mode or the account preference.
 * @param {Electron.Details} details
 */
function gpuGone(details) {
  if (!('type' in details) || details.type !== 'GPU' || !offscreenGame) return
  const page = offscreenGame.webContents
  stopSpout()
  tellOptions()
  note(`game: GPU process lost (${details.reason}); rebuilding offscreen rendering`)
  if (gpuReload) return
  dialog.showErrorBox('Washizu 图形进程中断', '游戏离屏画面已中断，正在重新加载。请核对游戏连接状态后继续。')
  gpuReload = setTimeout(() => {
    gpuReload = undefined
    if (page.isDestroyed()) return
    page.reload()
    startSpout()
    tellOptions()
  }, 1000)
}

/** @param {Electron.WebContents} page @returns {Promise<string>} the page's DevTools target id */
async function targetOf(page) {
  const tools = page.debugger
  tools.attach('1.3')
  try {
    const { targetInfo } = await tools.sendCommand('Target.getTargetInfo')
    return targetInfo.targetId
  } finally {
    tools.detach()
  }
}

/** Keep the dock stage's bounds for input and the pointer trail. */
function placeGame(/** @type {unknown} */ value) {
  const bounds = gameBounds(value)
  gameAt = offscreenGame && bounds.visible ? { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height } : null
  placeTrail()
}

/**
 * The pointer's trail: a transparent window over the game view that takes no
 * input — every click goes through to the game — and draws what the core says
 * of autoplay's pointer. A window of the shell's own, so nothing is added to
 * the game's page. It exists while the option is on, and shows while the game
 * view does.
 */
function placeTrail() {
  if (!options.pointer_trail || !window || !gamePage()) {
    trail?.destroy()
    trail = null
    return
  }
  if (!gameAt) {
    trail?.hide()
    return
  }
  trail ??= createTrail(window)
  trail.setBounds(trailBounds(window.getContentBounds(), gameAt))
  if (!trail.isVisible()) trail.showInactive()
}

function watchCursor() {
  if (!window || !core) return
  physicalPointer.observe({
    point: screen.getCursorScreenPoint(), now: Date.now(),
    area: manualPointerArea(window.getContentBounds(), gameAt, window.isFocused(), window.isMinimized()),
    page: OFFSCREEN, window: nativeWindowIdentity(window.getNativeWindowHandle()),
  })
}

/** @param {BaseWindow} owner */
function createTrail(owner) {
  const view = new BrowserWindow({
    parent: owner,
    transparent: true,
    backgroundColor: '#00000000',
    frame: false,
    hasShadow: false,
    focusable: false,
    skipTaskbar: true,
    resizable: false,
    movable: false,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    show: false,
    webPreferences: { preload: path.join(HERE, 'trail-preload.cjs'), sandbox: true, contextIsolation: true },
  })
  view.setIgnoreMouseEvents(true)
  const page = view.webContents
  page.setWindowOpenHandler(() => ({ action: 'deny' }))
  page.on('will-navigate', event => event.preventDefault())
  void view.loadFile(path.join(HERE, 'trail.html'))
  return view
}

function running() {
  if (!core) throw new Error('The core is not running.')
  return core
}

/**
 * What the interface can ask for. The commands and their arguments are the ones
 * the page uses under any host (`frontend/src/routes/Monitor.tsx`); the ones
 * about the window are answered here, and the rest are the core's.
 * @type {Record<string, (args: Record<string, unknown>) => Promise<unknown>>}
 */
const commands = {
  return_to_profiles: () => returnToProfiles(),
  get_monitor: () => running().call('get_monitor'),
  get_hand_events: ({ round }) => running().call('get_hand_events', { round: Number(round) }),
  get_history: () => running().call('get_history'),
  get_monitor_options: async () => options,
  set_monitor_autoplay: ({ enabled }) => running().call('set_monitor_autoplay', { enabled: enabled === true }),
  async set_monitor_lean({ enabled }) {
    const saved = /** @type {{ pointer_lean: boolean }} */ (await running().call('set_monitor_lean', { enabled: enabled === true }))
    options = { ...options, ...saved }
    tellOptions()
    return options
  },
  async set_monitor_confirm({ enabled }) {
    const saved = /** @type {{ quick_confirm: boolean }} */ (await running().call('set_monitor_confirm', { enabled: enabled === true }))
    options = { ...options, ...saved }
    tellOptions()
    return options
  },
  async set_monitor_auto_win({ enabled }) {
    const saved = /** @type {{ auto_win: boolean }} */ (await running().call('set_monitor_auto_win', { enabled: enabled === true }))
    options = { ...options, ...saved }
    tellOptions()
    return options
  },
  async set_monitor_continue({ enabled }) {
    const saved = /** @type {{ auto_continue: boolean }} */ (await running().call('set_monitor_continue', { enabled: enabled === true }))
    options = { ...options, ...saved }
    tellOptions()
    return options
  },
  async set_monitor_trail({ enabled }) {
    const saved = /** @type {{ pointer_trail: boolean }} */ (await running().call('set_monitor_trail', { enabled: enabled === true }))
    options = { ...options, ...saved }
    placeTrail()
    tellOptions()
    return options
  },
  async set_obs_output({ enabled }) {
    if (typeof enabled !== 'boolean') throw new Error('OBS output requires an enabled boolean.')
    if (enabled && !SPOUT_AVAILABLE) throw new Error('OBS output requires the Windows Spout2 sender.')
    saveObsPreference(DATA, enabled)
    obsEnabled = enabled
    startSpout()
    tellOptions()
    return options
  },
  /** Diagnostics for the fixed offscreen relay. */
  async get_view_stats() {
    if (!relay) return null
    return { frames: relay.frames, passed: relay.passed, takers: Object.fromEntries(relay.counts), obs: spout ? { state: spout.state, detail: spout.detail } : null }
  },
  async resize_monitor_game({ bounds }) {
    placeGame(bounds)
  },
  async open_monitor_game({ bounds }) {
    gameBounds(bounds)
    if (offscreenGame || !window) return
    offscreenGame = createOffscreenGame()
    const page = offscreenGame.webContents
    placeGame(bounds)
    try {
      await applyGameProxy(page.session, PROFILE.proxy)
      // The view starts blank; the core sends it to the game once it is listening.
      await page.loadURL('about:blank')
      await running().call('open_game', { port: cdpPort, target: await targetOf(page) })
    } catch (error) {
      if (offscreenGame?.webContents === page) {
        stopSpout()
        offscreenGame.destroy()
        offscreenGame = null
        relay = null
      }
      if (!page.isDestroyed()) page.close()
      throw error
    }
  },
}

/**
 * Before Electron is ready: the switches and the scheme it reads as it starts.
 * The entry module is waited for only as long as it has work pending, so the
 * one thing this waits on — finding a free port — is done in time.
 */
async function prepare() {
  // The game sees an ordinary Chromium: no application's name in the user agent.
  const named = new RegExp(` (Electron|${app.getName().replace(/[.*+?^${}()|[\]\\]/g, '\\$&')})/\\S+`, 'g')
  app.userAgentFallback = app.userAgentFallback.replace(named, '')
  cdpPort = Number(option('cdp-port')) || await freePort()
  note(`account ${PROFILE.id}: local DevTools port ${cdpPort}`)
  app.commandLine.appendSwitch('remote-debugging-port', String(cdpPort))
  app.commandLine.appendSwitch('remote-debugging-address', '127.0.0.1')
  // Room for the WebGL contexts the game's client leaves behind (see `WEBGL_CONTEXTS`).
  app.commandLine.appendSwitch('max-active-webgl-contexts', String(WEBGL_CONTEXTS))
  registerDockScheme()

  app.on('second-instance', showWindow)
  app.on('window-all-closed', () => app.quit())
  app.on('before-quit', event => {
    if (!core || profileReleased) { quitting = true; return }
    event.preventDefault()
    if (stoppingCore) return
    stoppingCore = true
    quitting = true
    if (process.connected) process.send?.({ type: 'profile-closing' }, error => { if (error) note(`account chooser notification failed: ${error.message}`) })
    // Only this profile's slot waits for this core/model cleanup ACK.
    void core.stop().then(() => new Promise(resolve => {
      if (process.connected && process.send) process.send({ type: 'profile-released' }, () => resolve(undefined))
      else resolve(undefined)
    })).then(() => { profileReleased = true; app.quit() }, error => {
      note(`profile cleanup failed: ${String(error)}`)
      if (process.connected && process.send) process.send({ type: 'profile-shutdown-failed', error: String(error) }, () => app.exit(1))
      else { dialog.showErrorBox('Washizu 无法关闭当前档案', String(error)); app.exit(1) }
    })
  })
  process.on('message', message => {
    if (message && typeof message === 'object' && 'type' in message && message.type === 'focus') {
      showWindow()
    }
  })
  // Losing the chooser must not terminate every otherwise-independent game.
  process.on('disconnect', () => note('account chooser disconnected; this client continues independently'))

  // The user's hand on the game's picture, when the game is drawn offscreen:
  // sent as it happens, answered with nothing.
  ipcMain.on('akagi:game-input', (event, input) => {
    const page = offscreenGame?.webContents
    if (!page || !dock || event.sender !== dock.webContents) return
    const action = toGame(input, OFFSCREEN)
    if (!action) return
    if ('event' in action) {
      // Keys go to the page that has the focus; the page is never shown, so it has to be given it.
      if (action.event.type === 'keyDown' || action.event.type === 'mouseDown') page.focus()
      page.sendInputEvent(action.event)
    } else if ('text' in action) {
      void page.insertText(action.text)
    } else {
      void clipboard.readText().then(text => { if (text && !page.isDestroyed()) void page.insertText(text.slice(0, TEXT_MAX)) })
    }
  })
  app.on('child-process-gone', (_event, details) => {
    note(`process gone: ${details.type}${details.name ? ` (${details.name})` : ''}, ${details.reason}, exit ${details.exitCode}`)
    gpuGone(details)
  })

  ipcMain.handle('akagi:invoke', async (event, command, args) => {
    try {
      if (!dock || event.sender !== dock.webContents) throw new Error('Only the interface can ask this.')
      const run = Object.hasOwn(commands, command) ? commands[command] : null
      if (!run) throw new Error(`Unknown command: ${command}`)
      return { ok: true, value: await run(args && typeof args === 'object' ? args : {}) }
    } catch (error) {
      return { ok: false, error: error instanceof Error ? error.message : String(error) }
    }
  })
}

/** Once Electron is ready: start the core, and open the window when the core is ready too. */
async function start() {
  Menu.setApplicationMenu(null)
  serveDock(DOCK_FILES)
  // Before the game's page opens: its service worker is let run from its first start (see `workers.js`).
  void keepWorkersRunning(cdpPort, note)

  const child = core = new Core(CORE, ['--home', DATA, '--engines', path.resolve(option('data'), 'engines'), '--account', PROFILE.id], { cwd: DATA })
  let gameEnded = true
  child.on('event', (name, payload) => {
    // The pointer's marks are the trail's alone, and come many a second. They
    // are in the game page's pixels; offscreen, the trail lies over the dock's
    // picture of the page, which is another size.
    if (name === 'pointer') trail?.webContents.send('trail:mark', offscreenGame && gameAt ? onView(/** @type {{ x: number, y: number }} */ (payload), gameAt, OFFSCREEN) : payload)
    else dock?.webContents.send('akagi:event', name, payload)
    if (name === 'monitor-updated') {
      // A game just ended: its page's garbage is collected a moment after.
      const ended = /** @type {{ game_ended?: boolean }} */ (payload)?.game_ended === true
      if (ended && !gameEnded) setTimeout(() => void collectGameGarbage(), COLLECT_AFTER_MS)
      gameEnded = ended
    }
  })
  child.on('exit', reason => fail('Washizu has stopped.', `${reason}\n\n${lastWords()}`))
  try {
    openWindow(await child.ready)
  } catch (error) {
    fail('Washizu could not start.', `${error instanceof Error ? error.message : error}\n${CORE}\n\n${lastWords()}`)
  }
}

/** Show the shared chooser without stopping capture, core, model or game. */
async function returnToProfiles() {
  if (!process.connected || !process.send) throw new Error('账号选择器连接已断开，请重新打开 Washizu 选择器。')
  await new Promise((resolve, reject) => process.send?.({ type: 'profile-show-chooser' }, error => error ? reject(error) : resolve(undefined)))
}

fs.mkdirSync(DATA, { recursive: true })
app.setPath('userData', path.join(DATA, 'browser'))
if (!hasChromiumLog(process.argv)) {
  // Chromium reads its logging switches before this code runs: start again with them, a file for this start.
  fs.mkdirSync(LOGS, { recursive: true })
  app.relaunch({ args: [...process.argv.slice(1), ...chromiumSwitches(LOGS, RUN), `--run=${RUN}`] })
  app.exit(0)
} else if (app.requestSingleInstanceLock()) {
  // Persist the one-way import only after owning this account's home.
  saveObsPreference(DATA, obsEnabled)
  prune(LOGS)
  note(`shell ${app.getVersion()} started: Electron ${process.versions.electron}, Chromium ${process.versions.chrome}`)
  await prepare()
  // Not awaited: Electron is not ready until this module has run to its end.
  void app.whenReady().then(start)
} else {
  // The profile belongs to the instance already running, which comes forward instead.
  app.exit(0)
}
