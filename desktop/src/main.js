// @ts-check
// A single Washizu entry. The chooser stays apart from each account's browser.
import { app, dialog } from 'electron'
import path from 'node:path'
import { defaultProfileRoot } from './profiles.js'

app.setName('Washizu')
const root = path.resolve(app.commandLine.getSwitchValue('data') || process.env.AKAGI_DATA || defaultProfileRoot({
  appData: app.getPath('appData'), executable: process.execPath, packaged: app.isPackaged,
}))
if (app.commandLine.hasSwitch('profile')) {
  app.commandLine.appendSwitch('data', root)
  await import('./game-main.js')
} else {
  const { startProfiles } = await import('./profile-main.js')
  // Electron becomes ready only after the entry module has finished. The
  // chooser registers its handlers now and opens its window after that.
  void startProfiles(root).catch(error => { dialog.showErrorBox('Washizu 无法打开账号档案', error instanceof Error ? error.message : String(error)); app.exit(1) })
}
