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
app.whenReady().then(async () => {
  frame = new BaseWindow({ width: 480, height: 240, show: false, title: 'Washizu window regression' })
  const view = new WebContentsView()
  frame.contentView.addChildView(view)
  view.setBounds({ x: 0, y: 0, width: 480, height: 240 })
  page = view.webContents
  await page.loadURL('data:text/html,<h3>Washizu window startup check</h3>')
  frame.show(); frame.focus()
  process.send?.({ type: 'profile-ready', visible: frame.isVisible() })
})
process.on('message', message => {
  if (message && typeof message === 'object' && 'type' in message && message.type === 'close-test') {
    process.send?.({ type: 'profile-released' }, () => { page?.close(); frame?.destroy(); app.quit() })
  }
})
