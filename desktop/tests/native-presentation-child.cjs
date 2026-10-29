// Actual Windows window startup, launched through ProfileSessions. No game/core.
const { app, BaseWindow, WebContentsView } = require('electron')
const path = require('node:path')
const fs = require('node:fs')
const root = app.commandLine.getSwitchValue('data')
fs.mkdirSync(path.join(root, 'browser'), { recursive: true })
app.setPath('userData', path.join(root, 'browser'))
app.commandLine.appendSwitch('disable-gpu')
/** @type {Electron.BaseWindow | undefined} */
let frame
/** @type {Electron.WebContents | undefined} */
let page
/** @type {typeof import('../src/window-frame.js').presentWindow | undefined} */
let present
app.whenReady().then(async () => {
  present = (await import('../src/window-frame.js')).presentWindow
  frame = new BaseWindow({ width: 480, height: 240, show: false, title: 'Washizu window regression' })
  const view = new WebContentsView()
  frame.contentView.addChildView(view)
  view.setBounds({ x: 0, y: 0, width: 480, height: 240 })
  page = view.webContents
  await page.loadURL('data:text/html,<h3>Washizu window startup check</h3>')
  present(frame)
  process.send?.({ type: 'profile-ready', visible: frame.isVisible() })
})
process.on('message', message => {
  if (message && typeof message === 'object' && 'type' in message && message.type === 'minimize-test') {
    frame?.minimize()
    setTimeout(() => process.send?.({ type: 'minimized-test', minimized: frame?.isMinimized() }), 100)
  }
  if (message && typeof message === 'object' && 'type' in message && message.type === 'focus') {
    if (frame) present?.(frame)
    setTimeout(() => process.send?.({ type: 'focus-test', visible: frame?.isVisible(), minimized: frame?.isMinimized(), focused: frame?.isFocused(), alwaysOnTop: frame?.isAlwaysOnTop() }), 100)
  }
  if (message && typeof message === 'object' && 'type' in message && message.type === 'close-test') {
    process.send?.({ type: 'profile-released' }, () => { page?.close(); frame?.destroy(); app.quit() })
  }
})
