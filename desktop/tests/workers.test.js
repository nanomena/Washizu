// @ts-check
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { WorkerRelease } from '../src/workers.js'

/** A release whose sent messages are kept, and answered by the test. */
function released() {
  /** @type {any[]} */
  const sent = []
  /** @type {string[]} */
  const letGo = []
  const release = new WorkerRelease(message => sent.push(message), url => letGo.push(url))
  return { release, sent, letGo }
}

const WORKER = { targetId: 'W1', type: 'service_worker', url: 'https://game.example/1/sw.js?v=4.0.51' }

test('every target is asked to be reported', () => {
  const { release, sent } = released()
  release.start()
  assert.deepEqual(sent, [{ id: 1, method: 'Target.setDiscoverTargets', params: { discover: true } }])
})

test('a service worker is attached to, told to run, and let go of only once it has answered', () => {
  const { release, sent, letGo } = released()
  release.receive({ method: 'Target.targetCreated', params: { targetInfo: WORKER } })
  assert.deepEqual(sent.at(-1), { id: 1, method: 'Target.attachToTarget', params: { targetId: 'W1', flatten: true } })

  release.receive({ id: 1, result: { sessionId: 'S1' } })
  assert.deepEqual(sent.at(-1), { id: 2, method: 'Runtime.runIfWaitingForDebugger', params: {}, sessionId: 'S1' })
  assert.equal(sent.length, 2, 'not let go of before it answers')

  release.receive({ id: 2, result: {}, sessionId: 'S1' })
  assert.deepEqual(sent.at(-1), { id: 3, method: 'Target.detachFromTarget', params: { sessionId: 'S1' } })
  assert.deepEqual(letGo, [WORKER.url])
})

test('a worker is let run once however often it is reported, and again when it starts anew', () => {
  const { release, sent } = released()
  release.receive({ method: 'Target.targetCreated', params: { targetInfo: WORKER } })
  release.receive({ method: 'Target.targetInfoChanged', params: { targetInfo: { ...WORKER, attached: true } } })
  assert.equal(sent.filter(m => m.method === 'Target.attachToTarget').length, 1)
  // Chromium stopped it for being idle; a start later is a new target.
  release.receive({ method: 'Target.targetDestroyed', params: { targetId: 'W1' } })
  release.receive({ method: 'Target.targetCreated', params: { targetInfo: { ...WORKER, targetId: 'W2' } } })
  assert.deepEqual(sent.at(-1).params, { targetId: 'W2', flatten: true })
})

test('pages, frames and other workers are left alone', () => {
  const { release, sent } = released()
  for (const type of ['page', 'iframe', 'worker', 'shared_worker', 'browser'])
    release.receive({ method: 'Target.targetCreated', params: { targetInfo: { ...WORKER, targetId: type, type } } })
  assert.deepEqual(sent, [])
})

test('a worker that could not be attached to is not told anything', () => {
  const { release, sent, letGo } = released()
  release.receive({ method: 'Target.targetCreated', params: { targetInfo: WORKER } })
  release.receive({ id: 1, error: { code: -32602, message: 'No target with given id found' } })
  assert.equal(sent.length, 1)
  assert.deepEqual(letGo, [])
})
