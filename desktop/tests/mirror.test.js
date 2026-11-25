// @ts-check
import test from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { MIRROR_POLL_MS, MirrorWatch, SEARCHING, mirrorPayload, nextMirror, pickSource } from '../src/mirror.js'
import { PLATFORMS } from '../src/profiles.js'

const TITLES = ['Example Client']
const game = { id: 'window:42:0', name: 'Example Client' }
const reopened = { id: 'window:43:0', name: 'Example Client' }

test('the game window is the first listed one whose title is exactly a client title', () => {
  assert.equal(pickSource([], TITLES), null)
  assert.equal(pickSource([{ id: 'window:1:0', name: 'Example Client - Notes' }, { id: 'screen:0:0', name: 'Entire Screen' }], TITLES), null)
  assert.deepEqual(pickSource([{ id: 'window:1:0', name: 'Other' }, game, reopened], TITLES), game)
  assert.deepEqual(pickSource([{ id: 'window:7:0', name: ' Example Client ' }], TITLES), { id: 'window:7:0', name: ' Example Client ' })
  assert.ok(PLATFORMS.HoloMahjong.windowTitles.length > 0)
})

test('the mirror moves only between its three explicit states', () => {
  const showing = nextMirror(SEARCHING, { type: 'listed', source: game })
  assert.deepEqual(showing, { state: 'showing', source: game })
  assert.equal(nextMirror(SEARCHING, { type: 'listed', source: null }), SEARCHING)
  assert.equal(nextMirror(showing, { type: 'listed', source: { ...game } }), showing, 'the same window keeps the same state')
  assert.deepEqual(nextMirror(showing, { type: 'listed', source: reopened }), { state: 'showing', source: reopened }, 'a new window of the client is shown instead')
  const lost = nextMirror(showing, { type: 'listed', source: null })
  assert.deepEqual(lost, { state: 'lost', source: null })
  assert.deepEqual(nextMirror(showing, { type: 'ended', id: game.id }), lost)
  assert.equal(nextMirror(showing, { type: 'ended', id: reopened.id }), showing, 'a stale report changes nothing')
  assert.equal(nextMirror(SEARCHING, { type: 'ended', id: game.id }), SEARCHING)
  assert.deepEqual(nextMirror(lost, { type: 'listed', source: game }), showing)
  assert.equal(nextMirror(lost, { type: 'listed', source: null }), SEARCHING, 'lost goes back to searching')
  assert.deepEqual(mirrorPayload(showing), { state: 'showing', source: game.id })
  assert.deepEqual(mirrorPayload(lost), { state: 'lost', source: null })
})

test('the watch reads the window list every two seconds, says each change once and stops cleanly', async () => {
  /** @type {Map<number, () => void>} */
  const timers = new Map()
  let next = 0
  /** @type {Array<{ id: string, name: string }> | Error} */
  let listed = []
  /** @type {string[]} */
  const states = []
  /** @type {string[]} */
  const log = []
  const watch = new MirrorWatch({
    list: async () => { if (listed instanceof Error) throw listed; return listed },
    titles: TITLES, onState: mirror => states.push(mirror.state), log: line => log.push(line),
    setTimer: (run, ms) => { assert.equal(ms, MIRROR_POLL_MS); timers.set(++next, run); return next },
    clearTimer: id => timers.delete(id),
  })
  const settle = () => new Promise(done => setImmediate(done))
  const tick = async () => {
    assert.equal(timers.size, 1)
    const [[id, run]] = timers
    timers.delete(id)
    run()
    await settle()
  }
  assert.equal(watch.granted(), null)
  watch.start()
  watch.start()
  await settle()
  assert.deepEqual(states, ['searching'])
  await tick()
  assert.deepEqual(states, ['searching'], 'no window yet: still searching, not said again')
  listed = [game]
  await tick()
  assert.deepEqual(states, ['searching', 'showing'])
  assert.deepEqual(watch.granted(), game)
  listed = new Error('enumeration refused')
  await tick()
  await tick()
  assert.deepEqual(states, ['searching', 'showing'], 'a failed read leaves the state as last seen')
  assert.equal(log.filter(line => line.includes('enumeration refused')).length, 1, 'a failing read is noted once')
  listed = [game]
  await tick()
  watch.ended(game.id)
  assert.deepEqual(states, ['searching', 'showing', 'lost'])
  assert.equal(watch.granted(), null)
  listed = []
  await tick()
  assert.deepEqual(states, ['searching', 'showing', 'lost', 'searching'])
  watch.stop()
  assert.equal(timers.size, 0)
  assert.equal(watch.granted(), null)
})

test('a native client account opens the proxy and a view-only mirror instead of a game page', () => {
  const probe = spawnSync(process.execPath, ['--experimental-vm-modules', fileURLToPath(new URL('./obs-output-probe.mjs', import.meta.url))], {
    env: { ...process.env, PROBE_PLATFORM: 'HoloMahjong' }, encoding: 'utf8', timeout: 15000,
  })
  assert.equal(probe.status, 0, `${probe.stdout}\n${probe.stderr}`)
  assert.match(probe.stdout, /Native client mirror: proxy, view-only window, explicit mirror states: passed/)
})
