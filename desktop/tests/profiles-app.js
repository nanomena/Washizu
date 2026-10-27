// Isolated Electron launcher fixture. Only tests can request native window
// lifecycle events; no such control is exposed to production renderers.
import { app } from 'electron'
import { startProfiles } from '../src/profile-main.js'

app.setName('Washizu')
const root = app.commandLine.getSwitchValue('data')
if (!root) throw new Error('The profile fixture requires an explicit temporary --data home')
const runtime = startProfiles(root)
let minimizeRequests = 0
void runtime.then(host => {
  if (!host) return
  const minimize = host.window.minimize.bind(host.window)
  host.window.minimize = () => { minimizeRequests++; minimize() }
})
void runtime.catch(error => { console.error(error); app.exit(1) })
process.on('message', async message => {
  if (!message || typeof message !== 'object' || !('id' in message) || !('action' in message)) return
  try {
    const host = await runtime
    if (!host) throw new Error('fixture lost the launcher lock')
    if (message.action === 'close-window') host.window.close()
    else if (message.action === 'quit') app.quit()
    else if (message.action === 'crash-client') {
      const child = host.sessions.clients.get('profile' in message ? String(message.profile) : '')?.child
      if (!child?.kill('SIGKILL')) throw new Error('fixture client is not running')
    } else if (message.action !== 'inspect') throw new Error('unknown fixture action')
    process.send?.({ id: message.id, value: {
      visible: !host.window.isDestroyed() && host.window.isVisible(),
      minimized: !host.window.isDestroyed() && host.window.isMinimized(),
      minimizeRequests,
      clients: [...host.sessions.clients.values()].map(({ id, state, child }) => ({ id, state, pid: child?.pid ?? null })),
    } })
  } catch (error) { process.send?.({ id: message.id, error: String(error) }) }
})
