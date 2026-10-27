// @ts-check
import assert from 'node:assert/strict'
import { once } from 'node:events'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { ProfileSessions } from '../src/profile-session.js'
import { ProfileStore } from '../src/profiles.js'

/** @param {import('node:test').TestContext} t */
function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-session-test-'))
  const store = new ProfileStore(root).load()
  const a = store.create('雀魂主号', 'Majsoul'), b = store.create('天凤主号', 'Tenhou')
  const settings = { executable: process.execPath, entry: [fileURLToPath(new URL('./profile-child.js', import.meta.url))], root }
  const sessions = new ProfileSessions(settings)
  t.after(async () => {
    await Promise.all([...sessions.clients.values()].map(async ({ child }) => {
      if (!child || child.exitCode !== null || child.signalCode !== null) return
      const closed = once(child, 'close').catch(() => {})
      child.kill(); await closed
    }))
    assert.equal(path.dirname(path.resolve(root)), path.resolve(os.tmpdir()))
    assert.ok(path.basename(root).startsWith('washizu-session-test-'))
    fs.rmSync(root, { recursive: true, force: true })
  })
  return { root, a, b, sessions, settings }
}

test('two profiles run concurrently and duplicate opens only focus their existing processes', async t => {
  const { root, a, b, sessions } = fixture(t)
  sessions.open(a.id)
  const first = sessions.clients.get(a.id)?.child
  assert.ok(first)
  const helloA = once(first, 'message')
  sessions.open(a.id)
  assert.equal(sessions.clients.get(a.id)?.child, first, 'duplicate during starting')
  sessions.open(b.id)
  const second = sessions.clients.get(b.id)?.child
  assert.ok(second)
  const [one, two] = await Promise.all([helloA, once(second, 'message')])
  assert.ok(one[0].args.includes(`--data=${root}`))
  assert.ok(one[0].args.includes(`--profile=${a.id}`))
  assert.ok(two[0].args.includes(`--profile=${b.id}`))
  assert.notEqual(first.pid, second.pid)
  assert.equal(sessions.liveCount, 2)
  assert.deepEqual(sessions.list().map(s => s.state), ['active', 'active'])
  const focused = once(first, 'message'); sessions.open(a.id)
  assert.equal((await focused)[0].type, 'focused')
  assert.equal(sessions.clients.get(a.id)?.child, first)
})

test('cleanup ACK alone cannot release A; closing A never changes B, and late A callbacks cannot claim a reopened slot', async t => {
  const { a, b, sessions } = fixture(t)
  sessions.open(a.id); sessions.open(b.id)
  const first = sessions.clients.get(a.id)?.child, second = sessions.clients.get(b.id)?.child
  assert.ok(first && second)
  await Promise.all([once(first, 'message'), once(second, 'message')])
  const released = once(first, 'message'); first.send({ type: 'release' }); await released
  assert.equal(sessions.clients.get(a.id)?.state, 'closing')
  assert.equal(sessions.clients.get(a.id)?.cleanup, 'acknowledged')
  assert.throws(() => sessions.open(a.id), /正在关闭/)
  assert.equal(sessions.clients.get(b.id)?.state, 'active')
  assert.equal(sessions.clients.get(b.id)?.child, second)
  const closed = once(sessions, 'closed'); first.send({ type: 'exit' })
  assert.deepEqual((await closed)[0], { id: a.id, error: '' })
  assert.equal(sessions.clients.has(a.id), false)
  sessions.open(a.id)
  const next = sessions.clients.get(a.id)?.child
  assert.ok(next && next !== first)
  first.emit('message', { type: 'profile-released' })
  first.emit('message', { type: 'profile-ready' })
  assert.equal(sessions.clients.get(a.id)?.state, 'starting')
  await once(next, 'message')
  assert.equal(sessions.clients.get(a.id)?.state, 'active')
  assert.equal(sessions.clients.get(b.id)?.child, second)
})

test('an unconfirmed crash locks only A and B can start normally afterward', async t => {
  const { a, b, sessions } = fixture(t)
  sessions.open(a.id)
  const first = sessions.clients.get(a.id)?.child
  assert.ok(first); await once(first, 'message')
  const closed = once(sessions, 'closed'); first.send({ type: 'unreleased-close' })
  assert.match((await closed)[0].error, /仅此档案保持锁定/)
  assert.equal(sessions.clients.get(a.id)?.child, null)
  assert.equal(sessions.clients.get(a.id)?.state, 'failed')
  assert.throws(() => sessions.open(a.id), /清理未确认/)
  sessions.open(b.id)
  const second = sessions.clients.get(b.id)?.child
  assert.ok(second); await once(second, 'message')
  assert.equal(sessions.clients.get(b.id)?.state, 'active')
  assert.equal(sessions.liveCount, 1)
})

test('a proven failure to spawn can retry its own profile and does not reserve a live client', async t => {
  const { root, a, sessions, settings } = fixture(t)
  settings.executable = path.join(root, 'missing')
  sessions.open(a.id)
  const [failure] = await once(sessions, 'closed')
  assert.match(failure.error, /ENOENT/)
  assert.equal(sessions.clients.get(a.id)?.cleanup, 'not-started')
  assert.equal(sessions.liveCount, 0)
  settings.executable = process.execPath
  sessions.open(a.id)
  const child = sessions.clients.get(a.id)?.child
  assert.ok(child); await once(child, 'message')
  assert.equal(sessions.clients.get(a.id)?.state, 'active')
})
