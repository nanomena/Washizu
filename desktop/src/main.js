// @ts-check
// The window Akagi runs in. One Chromium on every platform shows the game and
// the monitor's dock under it, in one 4:3 window, and serves the DevTools
// endpoint the core captures the game through. Everything else — the bridge,
// the tracker, the bot, autoplay, the monitor snapshot — is the core's
// (`src/headless`), a child process this file starts and talks to in JSON lines.
//
//   --data=<dir>      where everything is kept (default: <appData>/akagi-desktop):
//                     config.toml, bots, logs and history, and the browser profile
//   --core=<file>     the core's executable (default: the packaged one)
//   --dock=<dir>      the built interface (default: the packaged one)
//   --cdp-port=<n>    the local DevTools port (default: any free port)
//   --spout=<file>    the Spout sender (Windows; default: the packaged one)
// Each can also be given as AKAGI_DATA, AKAGI_CORE, AKAGI_DOCK, AKAGI_CDP_PORT, AKAGI_SPOUT.
//
// The game is shown one of two ways, as `view.json` in the data folder says
// (the settings tab writes it; it takes effect at the next start):
//   window     the game's page is a view in this window, where the dock's stage is
//   offscreen  the game's page is drawn offscreen, always at 1280 × 720 CSS
//              pixels and twice that in pixels; its frames go on the GPU to the
//              dock, which shows them on its stage and sends the user's mouse
//              and keyboard back (`relay.js`, `input.js`), and — on Windows,
//              when asked — to a Spout sender for a streaming program. The
//              window's size, screen and scale then never touch the game.

import { BaseWindow, BrowserWindow, Menu, WebContentsView, app, clipboard, dialog, ipcMain, net, protocol, screen, sharedTexture } from 'electron'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import { createServer } from 'node:net'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { Board } from './board.js'
import { Core } from './core.js'
import { Feed } from './feed.js'
import { FrameSource } from './frames.js'
import { TEXT_MAX, toGame } from './input.js'
import { MIN, RATIO, conform, framedRatio, gameBounds, handOnGame, onGame, onView, openingSize, restoredBounds, settle, trailBounds } from './layout.js'
import { policyFor } from './policy.js'
import { Relay, SpoutTaker, pageTaker } from './relay.js'
import { STREAM_FPS, StreamServer, newToken, streamUrl } from './stream.js'

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

const DATA = path.resolve(option('data') || path.join(app.getPath('appData'), 'akagi-desktop'))
const CORE = path.resolve(option('core') || (app.isPackaged ? path.join(process.resourcesPath, 'core', CORE_NAME) : path.join(HERE, '..', '..', 'target', 'release', CORE_NAME)))
const DOCK_FILES = path.resolve(option('dock') || (app.isPackaged ? path.join(process.resourcesPath, 'dock') : path.join(HERE, '..', '..', 'frontend', 'dist')))
const WINDOW_STATE = path.join(DATA, 'window.json')
/** The user's own pages for the streaming program, served beside the stream while it is. */
const STREAM_PAGES = path.join(DATA, 'stream-pages')
/** How the game is shown (`window` or `offscreen`), and whether a Spout sender is run; the shell's own, kept here. */
const VIEW_STATE = path.join(DATA, 'view.json')
const SPOUT = path.resolve(option('spout') || (app.isPackaged ? path.join(process.resourcesPath, 'spout', 'spout-sender.exe') : path.join(HERE, '..', 'native', 'spout', 'build', 'spout-sender.exe')))
/** The name a streaming program finds the game's frames under. */
const SPOUT_NAME = 'Akagi'
/**
 * The game rendered offscreen: its page's size in CSS pixels, how many pixels
 * each is drawn with, and how many frames a second. Mahjong Soul draws its
 * canvas at 2560 × 1440, so at twice 1280 × 720 every one of its pixels is a
 * pixel of the frame; a streaming program scales that down to its output.
 */
const OFFSCREEN = Object.freeze({ width: 1280, height: 720, scale: 2, fps: 60 })
/** The page offscreen comes back after the GPU process is lost this many times in `GPU_LOSS_WINDOW_MS`; after that, the window view is used from the next start. */
const GPU_LOSSES = 2
const GPU_LOSS_WINDOW_MS = 10 * 60 * 1000

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
/** The game's page in this window (the window view). */
/** @type {WebContentsView | null} */
let game = null
/** The game's page drawn offscreen, in a window that is never shown. */
/** @type {BrowserWindow | null} */
let offscreenGame = null
/** @type {Relay | null} */
let relay = null
/** @type {SpoutTaker | null} */
let spout = null
/** When the GPU process was last lost while the game was drawn offscreen. */
/** @type {number[]} */
let gpuLosses = []
/** @type {Core | null} */
let core = null
/** Autoplay's pointer, drawn over the game while the option is on. */
/** @type {BrowserWindow | null} */
let trail = null
/** The system cursor, as last read, when it last moved, and whether the user's hand is on the game. */
/** @type {{ x: number, y: number } | null} */
let cursorAt = null
let cursorMovedAt = 0
let userActive = false
/** How long the cursor has to stand still before it counts as at rest. How
 *  long autoplay then waits before it has the pointer back is the core's to
 *  draw (`humanize::HAND_BACK`). */
const USER_STILL_MS = 150
/** Where the game view is in the window's content, while it is shown. */
/** @type {import('./layout.js').Rect | null} */
let gameAt = null
/**
 * The window's options as the core last saved them, the build's version, and
 * where the stream is served while it is.
 * @typedef {{ always_on_top: boolean, pointer_trail?: boolean, pointer_lean?: boolean,
 *   quick_confirm?: boolean, riichi_auto_win?: boolean, auto_continue?: boolean,
 *   stream_output?: boolean, stream_port?: number, stream_token?: string, stream_url?: string, version?: string,
 *   game_view?: GameView, game_view_now?: GameView, spout_output?: boolean, spout_available?: boolean,
 *   spout_state?: string, spout_name?: string }} Options
 * @typedef {'window' | 'offscreen'} GameView
 */
/** @type {Options} */
let options = { always_on_top: false }
/** How the game is to be shown, and whether to send it to a streaming program over Spout, as last chosen. */
const viewChoice = readView()
/** How the game is shown in this run: chosen at the start, kept to the end. */
const GAME_VIEW = viewChoice.game_view
/** Whether a Spout sender can run here. */
const SPOUT_AVAILABLE = process.platform === 'win32' && fs.existsSync(SPOUT)
/** The game offered to a streaming program: served while the option is on, drawn for it while anyone watches. */
/** @type {StreamServer | null} */
let stream = null
/** What the table is doing, kept from the core's snapshots for the stream's `/feed`; nothing about the bot. */
const feed = new Feed({ readResults: () => running().call('get_history'), log: message => console.warn(message) })
/** What the pages beside the stream share; kept while the application runs, through the stream going off and on. */
const board = new Board()
/** @type {FrameSource | null} */
let frames = null
/** @type {NodeJS.Timeout | undefined} */
let framesIdle
/** The stream being served at start: the interface is told the options once it is. */
let streamStarting = Promise.resolve()
/** Whether the core has been given the game view: frames are only taken from a view that is set up. */
let gameOpen = false
let cdpPort = 0
let quitting = false

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

/** Serve the built interface, and nothing outside its folder. */
function serveDock() {
  protocol.handle('app', async request => {
    const url = new URL(request.url)
    const file = path.join(DOCK_FILES, decodeURIComponent(url.pathname === '/' ? '/index.html' : url.pathname))
    const inside = path.relative(DOCK_FILES, file)
    if (url.host !== 'dock' || inside.startsWith('..') || path.isAbsolute(inside)) return new Response(null, { status: 404 })
    try {
      if (file.endsWith('.html')) {
        const html = await fs.promises.readFile(file, 'utf8')
        return new Response(html, { headers: { 'Content-Type': 'text/html; charset=utf-8', 'Content-Security-Policy': policyFor(html) } })
      }
      return await net.fetch(pathToFileURL(file).toString())
    } catch {
      return new Response(null, { status: 404 })
    }
  })
}

/** @returns {unknown} the JSON in `file`, or null */
function readJson(/** @type {string} */ file) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')) } catch { return null }
}

/** @returns {{ game_view: GameView, spout: boolean }} */
function readView() {
  const saved = /** @type {Record<string, unknown> | null} */ (readJson(VIEW_STATE))
  return { game_view: saved?.game_view === 'offscreen' ? 'offscreen' : 'window', spout: saved?.spout === true }
}

function saveView() {
  try { fs.writeFileSync(VIEW_STATE, JSON.stringify(viewChoice)) } catch (error) { console.warn(`view: not saved: ${error instanceof Error ? error.message : error}`) }
}

/** The options about the game's view, as the interface is told them. */
function viewOptions() {
  return {
    game_view: viewChoice.game_view, game_view_now: GAME_VIEW, spout_output: viewChoice.spout, spout_available: SPOUT_AVAILABLE,
    spout_state: GAME_VIEW !== 'offscreen' || !viewChoice.spout ? 'off' : spout?.state ?? 'starting', spout_name: SPOUT_NAME,
  }
}

/** Tell the interface the options again, for a change it did not ask for. */
function tellOptions() {
  options = { ...options, ...viewOptions() }
  dock?.webContents.send('akagi:event', 'monitor-options', options)
}

/** @param {BaseWindow} frame */
function contentSize(frame) {
  const [width, height] = frame.getContentSize()
  return { width, height }
}

/**
 * Keep the window's content on the canvas's ratio while an edge is dragged.
 * macOS and X11 hold a content ratio themselves. Windows holds the ratio of the
 * whole window, title bar included, so there the ratio asked for is the one
 * that puts the content right at its present width, said again as that changes.
 * Wherever a drag ends, the content is then put exactly on the ratio.
 * @param {BaseWindow} frame
 */
function holdRatio(frame) {
  if (process.platform === 'win32') {
    const [outerWidth, outerHeight] = frame.getSize()
    const inner = contentSize(frame)
    const border = { width: outerWidth - inner.width, height: outerHeight - inner.height }
    const say = () => frame.setAspectRatio(framedRatio(contentSize(frame).width, border))
    say()
    frame.on('resize', say)
  } else {
    frame.setAspectRatio(RATIO)
  }

  let before = contentSize(frame)
  const rest = () => {
    const now = contentSize(frame)
    const exact = settle(before, now)
    if (Math.abs(exact.width - now.width) > 1 || Math.abs(exact.height - now.height) > 1) frame.setContentSize(exact.width, exact.height)
    before = exact
  }
  if (process.platform === 'linux') {
    // No event says a drag has ended here, and Wayland holds no ratio at all:
    // the size is at rest once it has stopped changing.
    /** @type {NodeJS.Timeout | undefined} */
    let quiet
    frame.on('resize', () => { clearTimeout(quiet); quiet = setTimeout(rest, 250) })
  } else {
    frame.on('resized', rest)
  }
}

/** @param {import('./core.js').Ready} ready */
function openWindow(ready) {
  options = { ...ready.monitor, version: ready.version, ...viewOptions() }
  if (options.stream_output) {
    // Served from the start, at the address the streaming program already has.
    streamStarting = startStream().catch(error => {
      console.warn(`stream: not served: ${error instanceof Error ? error.message : error}`)
      options = { ...options, stream_output: false }
    })
  }
  const saved = restoredBounds(readJson(WINDOW_STATE), screen.getAllDisplays().map(display => display.workArea))
  const size = saved ?? openingSize(screen.getPrimaryDisplay().workAreaSize)
  const frame = window = new BaseWindow({
    ...(saved ? { x: saved.x, y: saved.y } : {}),
    width: size.width,
    height: size.height,
    useContentSize: true,
    minWidth: MIN.width,
    minHeight: MIN.height,
    title: `${ready.brand} · 雀魂`,
    // A maximized or full-screen window cannot keep the canvas's ratio.
    maximizable: false,
    fullscreenable: false,
    alwaysOnTop: ready.monitor.always_on_top,
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
  page.once('did-finish-load', () => frame.show())
  page.on('did-fail-load', (_event, _code, description, _url, isMainFrame) => {
    if (isMainFrame) fail('The interface could not be loaded.', `${description}\n${DOCK_FILES}`)
  })
  void page.loadURL(`${DOCK}index.html?view=monitor`)

  frame.on('close', () => {
    const [x, y] = frame.getPosition()
    try { fs.writeFileSync(WINDOW_STATE, JSON.stringify({ x, y, ...contentSize(frame) })) } catch { /* where the window was is a convenience */ }
  })
  frame.on('closed', () => {
    frames?.stop()
    void stream?.close()
    feed.close()
    spout?.stop()
    // Views outlive the window they were in unless they are closed.
    game?.webContents.close()
    offscreenGame?.destroy()
    view.webContents.close()
    trail?.destroy()
    window = dock = game = offscreenGame = trail = null
    relay = spout = null
    app.quit()
  })
}

/** The game's page, whichever way it is shown. */
function gamePage() {
  return game?.webContents ?? offscreenGame?.webContents ?? null
}

/** The game view: its own session, no way into the shell, and never put to sleep. */
function createGame() {
  // `backgroundThrottling: false` keeps the page live — drawing, and taking the
  // input autoplay sends it — while the window is covered or minimized.
  const view = new WebContentsView({ webPreferences: { partition: GAME_PARTITION, sandbox: true, contextIsolation: true, backgroundThrottling: false } })
  view.setBackgroundColor('#000000')
  guardGame(view.webContents)
  return view
}

/**
 * The game drawn offscreen, in a window that is never shown. The page's size
 * comes from that window, through the system's scaling of a window that has
 * a frame: without one, and with its corner on a whole number of pixels at any
 * scale, it is exactly the size asked for (checked again on every frame).
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
  const resize = () => win.setBounds({ x: 0, y: 0, width: OFFSCREEN.width, height: OFFSCREEN.height })
  const fanout = relay = new Relay({ expected: { width: OFFSCREEN.width * OFFSCREEN.scale, height: OFFSCREEN.height * OFFSCREEN.scale }, onSize: resize, log: message => console.warn(message) })
  fanout.add(pageTaker({ sharedTexture, frame: () => dock?.webContents.mainFrame ?? null, log: message => console.warn(message) }))
  page.on('paint', event => { if (event.texture) fanout.paint(event.texture) })
  // The page draws its own cursor no more: the dock's stage shows the one it asks for.
  page.on('cursor-changed', (_event, type) => dock?.webContents.send('akagi:event', 'game-cursor', type))
  screen.on('display-metrics-changed', resize)
  win.on('closed', () => screen.off('display-metrics-changed', resize))
  startSpout()
  return win
}

/** Run the Spout sender while it is asked for and the game is drawn offscreen, and not otherwise. */
function startSpout() {
  if (!relay || !viewChoice.spout || !SPOUT_AVAILABLE) {
    stopSpout()
    return
  }
  if (spout) return
  spout = new SpoutTaker({ exe: SPOUT, pid: process.pid, sender: SPOUT_NAME, spawn, log: message => console.warn(message), onState: () => tellOptions() })
  relay.add(spout)
}

function stopSpout() {
  if (!spout) return
  relay?.delete(spout)
  spout.stop()
  spout = null
}

/** What the game's page may do, either way it is shown. @param {Electron.WebContents} page */
function guardGame(page) {
  const web = (/** @type {string} */ url) => /^https?:\/\//i.test(url)
  page.on('will-navigate', (event, url) => { if (!web(url)) event.preventDefault() })
  // Signing in through another service opens a window, and that window has to
  // share the game's session to hand the sign-in back. It is an ordinary
  // window, even when the game is drawn offscreen.
  page.setWindowOpenHandler(({ url }) => web(url)
    ? { action: 'allow', overrideBrowserWindowOptions: { width: 960, height: 720, autoHideMenuBar: true, show: true, webPreferences: { sandbox: true, contextIsolation: true, offscreen: false } } }
    : { action: 'deny' })
  page.on('render-process-gone', () => page.reload())
  page.session.setPermissionRequestHandler((_contents, permission, allow) => allow(GAME_PERMISSIONS.has(permission)))
  page.session.setPermissionCheckHandler((_contents, permission) => GAME_PERMISSIONS.has(permission))
}

/**
 * A page drawn offscreen draws nothing more once the GPU process has gone,
 * though it runs on; only loading it again brings its frames back (the game
 * then reconnects to a table it was at). If that keeps happening, the window
 * view is used from the next start.
 * @param {Electron.Details} details
 */
function gpuGone(details) {
  if (!('type' in details) || details.type !== 'GPU' || !offscreenGame) return
  const now = Date.now()
  gpuLosses = [...gpuLosses.filter(at => now - at < GPU_LOSS_WINDOW_MS), now]
  console.warn(`game: the GPU process went (${details.reason}); loading the game again`)
  if (gpuLosses.length >= GPU_LOSSES && viewChoice.game_view === 'offscreen') {
    console.warn('game: the window view is used from the next start')
    viewChoice.game_view = 'window'
    saveView()
    tellOptions()
  }
  // A moment for the new GPU process to come up.
  setTimeout(() => offscreenGame?.webContents.reload(), 1000)
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

/** Put the game view where the page says its stage is, or out of sight. */
function placeGame(/** @type {unknown} */ value) {
  const bounds = gameBounds(value)
  if (offscreenGame) {
    // The stage shows the frames; where it is matters only to the trail and the user's hand.
    gameAt = bounds.visible ? { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height } : null
    placeTrail()
    watchStream()
    return
  }
  if (!game) return
  game.setVisible(bounds.visible)
  if (bounds.visible) game.setBounds({ x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height })
  gameAt = bounds.visible ? { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height } : null
  placeTrail()
  watchStream()
}

/**
 * Take frames while anyone watches and the game view is on screen, and not
 * otherwise. Being left is given a moment first, so that the streaming program
 * reloading its page does not stop and start them.
 */
function watchStream() {
  clearTimeout(framesIdle)
  const page = gamePage()
  if (stream && stream.watchers > 0 && page && gameOpen && gameAt) {
    frames ??= new FrameSource(page.debugger, { onFrame: jpeg => stream?.push(jpeg), fps: STREAM_FPS, log: message => console.warn(message) })
    frames.start()
  } else if (frames?.running) {
    if (stream?.watchers === 0 && gameAt) framesIdle = setTimeout(() => frames?.stop(), 3000)
    else frames.stop()
  }
}

/** The options as the interface is told them: with the stream's address while it is served. */
function withStream(/** @type {Options} */ saved) {
  return { ...saved, stream_url: stream && saved.stream_output && saved.stream_token ? streamUrl(stream.port, saved.stream_token) : undefined }
}

/**
 * Serve the stream at the address kept, or at a new one if there is none or
 * its port has been taken since. The core keeps where it ended up.
 */
async function startStream() {
  const token = options.stream_token || newToken()
  const server = stream ?? new StreamServer({ token, onWatchers: watchStream, feed, board, pages: STREAM_PAGES })
  const port = await server.listen(options.stream_port ?? 0)
  stream = server
  if (!options.stream_output || port !== options.stream_port || token !== options.stream_token) {
    try {
      const saved = /** @type {Options} */ (await running().call('set_monitor_stream', { enabled: true, port, token }))
      options = { ...options, ...saved }
    } catch (error) {
      await stopStream()
      throw error
    }
  }
  options = withStream(options)
}

async function stopStream() {
  clearTimeout(framesIdle)
  frames?.stop()
  frames = null
  const server = stream
  stream = null
  await server?.close()
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
  const point = screen.getCursorScreenPoint()
  const now = Date.now()
  if (!cursorAt || point.x !== cursorAt.x || point.y !== cursorAt.y) {
    cursorAt = point
    cursorMovedAt = now
  }
  const area = gameAt && !window.isMinimized() ? trailBounds(window.getContentBounds(), gameAt) : null
  const active = handOnGame(point, area, cursorMovedAt, now, USER_STILL_MS)
  if (active !== userActive) {
    userActive = active
    // Resting over the game, the user's own mouse left the game's pointer where
    // it is: autoplay goes on from there, not from where it last put it.
    const at = active ? null : onGame(point, area, offscreenGame ? OFFSCREEN : null)
    core.call('set_user_pointer', at ? { active, x: at.x, y: at.y } : { active }).catch(() => {})
  }
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
  get_monitor: () => running().call('get_monitor'),
  get_hand_events: ({ round }) => running().call('get_hand_events', { round: Number(round) }),
  get_history: () => running().call('get_history'),
  get_monitor_options: async () => {
    await streamStarting
    return options
  },
  set_monitor_autoplay: ({ enabled }) => running().call('set_monitor_autoplay', { enabled: enabled === true }),
  async set_monitor_lean({ enabled }) {
    const saved = /** @type {{ pointer_lean: boolean }} */ (await running().call('set_monitor_lean', { enabled: enabled === true }))
    options = { ...options, ...saved }
    return options
  },
  async set_monitor_confirm({ enabled }) {
    const saved = /** @type {{ quick_confirm: boolean }} */ (await running().call('set_monitor_confirm', { enabled: enabled === true }))
    options = { ...options, ...saved }
    return options
  },
  async set_monitor_auto_win({ enabled }) {
    const saved = /** @type {{ riichi_auto_win: boolean }} */ (await running().call('set_monitor_auto_win', { enabled: enabled === true }))
    options = { ...options, ...saved }
    return options
  },
  async set_monitor_continue({ enabled }) {
    const saved = /** @type {{ auto_continue: boolean }} */ (await running().call('set_monitor_continue', { enabled: enabled === true }))
    options = { ...options, ...saved }
    return options
  },
  async set_monitor_trail({ enabled }) {
    const saved = /** @type {{ pointer_trail: boolean }} */ (await running().call('set_monitor_trail', { enabled: enabled === true }))
    options = { ...options, ...saved }
    placeTrail()
    return options
  },
  async set_monitor_stream({ enabled }) {
    if (enabled === true) {
      await startStream()
      return options
    }
    await stopStream()
    const saved = /** @type {Options} */ (await running().call('set_monitor_stream', { enabled: false, port: options.stream_port ?? 0, token: options.stream_token ?? '' }))
    options = withStream({ ...options, ...saved })
    return options
  },
  /** How the game is shown from the next start: `window` or `offscreen`. */
  async set_monitor_game_view({ view: chosen }) {
    if (chosen !== 'window' && chosen !== 'offscreen') throw new Error('Unknown game view.')
    viewChoice.game_view = chosen
    saveView()
    options = { ...options, ...viewOptions() }
    return options
  },
  /** Whether the game's frames are sent to a streaming program over Spout, while the game is drawn offscreen. */
  async set_monitor_spout({ enabled }) {
    viewChoice.spout = enabled === true
    saveView()
    startSpout()
    options = { ...options, ...viewOptions() }
    return options
  },
  /** How the game's frames are flowing, when it is drawn offscreen. */
  async get_view_stats() {
    if (!relay) return null
    return { frames: relay.frames, passed: relay.passed, takers: Object.fromEntries(relay.counts), spout: spout ? { state: spout.state, detail: spout.detail } : null }
  },
  async copy_stream_url() {
    if (!options.stream_url) throw new Error('The stream is not on.')
    clipboard.writeText(options.stream_url)
  },
  async set_monitor_pinned({ pinned }) {
    const saved = /** @type {{ always_on_top: boolean }} */ (await running().call('set_monitor_pinned', { pinned: pinned === true }))
    options = { ...options, ...saved }
    window?.setAlwaysOnTop(options.always_on_top)
    return options
  },
  async resize_monitor_game({ bounds }) {
    placeGame(bounds)
  },
  async open_monitor_game({ bounds }) {
    gameBounds(bounds)
    if (game || offscreenGame || !window) return
    /** @type {Electron.WebContents} */
    let page
    if (GAME_VIEW === 'offscreen') {
      offscreenGame = createOffscreenGame()
      page = offscreenGame.webContents
    } else {
      game = createGame()
      window.contentView.addChildView(game)
      page = game.webContents
    }
    placeGame(bounds)
    try {
      // The view starts blank; the core sends it to the game once it is listening.
      await page.loadURL('about:blank')
      await running().call('open_game', { port: cdpPort, target: await targetOf(page) })
      gameOpen = true
      watchStream()
    } catch (error) {
      if (game?.webContents === page) {
        window?.contentView.removeChildView(game)
        game = null
      }
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
  app.commandLine.appendSwitch('remote-debugging-port', String(cdpPort))
  app.commandLine.appendSwitch('remote-debugging-address', '127.0.0.1')
  protocol.registerSchemesAsPrivileged([{ scheme: 'app', privileges: { standard: true, secure: true, supportFetchAPI: true } }])

  app.on('second-instance', () => {
    if (window?.isMinimized()) window.restore()
    window?.focus()
  })
  app.on('window-all-closed', () => app.quit())
  app.on('before-quit', () => { quitting = true })
  app.on('will-quit', () => core?.close())

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
  app.on('child-process-gone', (_event, details) => gpuGone(details))

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
  // No menu bar under the title; macOS keeps the application menu it expects.
  Menu.setApplicationMenu(process.platform === 'darwin'
    ? Menu.buildFromTemplate([{ role: 'appMenu' }, { role: 'editMenu' }, { role: 'windowMenu' }])
    : null)
  serveDock()

  const child = core = new Core(CORE, ['--home', DATA], { cwd: DATA })
  child.on('event', (name, payload) => {
    // The pointer's marks are the trail's alone, and come many a second. They
    // are in the game page's pixels; offscreen, the trail lies over the dock's
    // picture of the page, which is another size.
    if (name === 'pointer') trail?.webContents.send('trail:mark', offscreenGame && gameAt ? onView(/** @type {{ x: number, y: number }} */ (payload), gameAt, OFFSCREEN) : payload)
    else dock?.webContents.send('akagi:event', name, payload)
    if (name === 'monitor-updated') feed.update(payload)
  })
  child.on('exit', reason => fail('Akagi has stopped.', `${reason}\n\n${lastWords()}`))
  try {
    openWindow(await child.ready)
  } catch (error) {
    fail('Akagi could not start.', `${error instanceof Error ? error.message : error}\n${CORE}\n\n${lastWords()}`)
  }
}

fs.mkdirSync(DATA, { recursive: true })
app.setPath('userData', path.join(DATA, 'browser'))
if (app.requestSingleInstanceLock()) {
  await prepare()
  // Not awaited: Electron is not ready until this module has run to its end.
  void app.whenReady().then(start)
} else {
  // The profile belongs to the instance already running, which comes forward instead.
  app.exit(0)
}
