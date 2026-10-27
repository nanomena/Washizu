// @ts-check
import assert from 'node:assert/strict'
import { once } from 'node:events'
import path from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { Core } from '../src/core.js'

const STAND_IN = path.join(path.dirname(fileURLToPath(import.meta.url)), 'stand-in-core.js')
const start = (/** @type {string} */ mode) => new Core(process.execPath, [STAND_IN, '--mode', mode])

test('the core says it is ready, answers by id, and refuses by message', async () => {
  const core = start('ok')
  assert.deepEqual(await core.ready, { brand: 'Stand-in', platform: 'Majsoul', monitor: { always_on_top: false }, version: '0.0.0' })
  // Asked together, answered each to its own.
  const [first, second] = await Promise.all([core.call('echo', { n: 1 }), core.call('echo', { n: 2 })])
  assert.deepEqual([first, second], [{ n: 1 }, { n: 2 }])
  await assert.rejects(core.call('refuse'), { message: 'unknown method: refuse' })
  core.close()
  await once(core, 'exit')
})

test('what the core says unasked arrives as an event, not as an answer', async () => {
  const core = start('ok')
  await core.ready
  const said = once(core, 'event')
  assert.equal(await core.call('nudge'), null)
  assert.deepEqual(await said, ['monitor-updated', { sequence: 7 }])
  core.close()
  await once(core, 'exit')
})

test('closing its stdin is all it takes to end the core', async () => {
  const core = start('ok')
  await core.ready
  const gone = once(core, 'exit')
  core.close()
  assert.deepEqual(await gone, ['The core stopped (exit 0).'])
  await assert.rejects(core.call('echo'), { message: 'The core is not running.' })
})

test('an account shutdown waits for the core to release its pipes', async () => {
  const core = start('ok')
  await core.ready
  await core.stop()
  assert.equal(core.gone, true)
  await core.stop()
  await assert.rejects(core.call('echo'), /not running/)
})

test('a core that dies fails what was waiting on it and keeps its last words', async () => {
  const core = start('crash')
  await core.ready
  await assert.rejects(core.call('echo'), { message: 'The core stopped (exit 3).' })
  assert.match(core.stderr, /stand-in core: giving up/)
})

test('missing or failed service-release acknowledgements cannot complete an account shutdown', async () => {
  for (const mode of ['shutdown-missing', 'shutdown-failure']) {
    const core = start(mode)
    await core.ready
    await assert.rejects(core.stop(), mode === 'shutdown-missing' ? /did not confirm/ : /could not be reaped/)
    assert.equal(core.gone, true)
  }
})

test('a core that cannot be started says so', async () => {
  const core = new Core(path.join(path.dirname(STAND_IN), 'no-such-core'), [])
  await assert.rejects(core.ready, /The core could not be started/)
})
