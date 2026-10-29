// Run on Windows with Electron installed, or ELECTRON_OVERRIDE_DIST_PATH set.
// Exercise the actual launcher and first native show, not a mocked show() call.
import assert from 'node:assert/strict'
import { once } from 'node:events'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { ProfileSessions } from '../src/profile-session.js'

assert.equal(process.platform, 'win32', 'This regression checks Windows startup visibility')
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-native-presentation-'))
const sessions = new ProfileSessions({ executable: createRequire(import.meta.url)('electron'), entry: [fileURLToPath(new URL('./native-presentation-child.cjs', import.meta.url))], root })
let child
try {
  sessions.open('existing')
  child = sessions.clients.get('existing')?.child
  assert.ok(child)
  const [ready] = await once(child, 'message', { signal: AbortSignal.timeout(15000) })
  assert.equal(ready.type, 'profile-ready')
  assert.equal(ready.visible, true, 'First Open must show the native window without another focus request')
  assert.equal(sessions.list()[0].state, 'active')
  const minimized = once(child, 'message', { signal: AbortSignal.timeout(5000) })
  child.send({ type: 'minimize-test' })
  assert.equal((await minimized)[0].minimized, true)
  const focused = once(child, 'message', { signal: AbortSignal.timeout(5000) })
  sessions.focus('existing')
  const [shown] = await focused
  assert.equal(shown.type, 'focus-test')
  assert.equal(shown.visible, true)
  assert.equal(shown.minimized, false)
  assert.equal(shown.focused, true)
  assert.equal(shown.alwaysOnTop, false, 'raising does not pin the account above other applications')
  const released = once(sessions, 'closed', { signal: AbortSignal.timeout(10000) })
  child.send({ type: 'close-test' })
  await released
  assert.equal(sessions.liveCount, 0)
  console.log('Actual Windows launcher: first Open visible; focus restores and raises; permanent topmost unchanged; cleanup acknowledged.')
} finally {
  if (child && child.exitCode === null && child.signalCode === null) {
    const closed = once(child, 'close'); child.kill(); await closed
  }
  assert.equal(path.dirname(path.resolve(root)), path.resolve(os.tmpdir()))
  assert.ok(path.basename(root).startsWith('washizu-native-presentation-'))
  fs.rmSync(root, { recursive: true, force: true })
}
