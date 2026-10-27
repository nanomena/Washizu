// @ts-check
import assert from 'node:assert/strict'
import { once } from 'node:events'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { ProfileSession } from '../src/profile-session.js'
import { ProfileStore } from '../src/profiles.js'

test('account ownership is held until the old process closes, then the next profile starts clean', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-session-test-'))
  const store = new ProfileStore(root).load()
  const first = store.create('雀魂主号', 'Majsoul')
  const second = store.create('天凤主号', 'Tenhou')
  const child = fileURLToPath(new URL('./profile-child.js', import.meta.url))
  const session = new ProfileSession({ executable: process.execPath, entry: [child], root })
  t.after(() => { session.child?.kill(); assert.ok(root.startsWith(path.join(os.tmpdir(), 'washizu-session-test-'))); fs.rmSync(root, { recursive: true, force: true }) })
  session.open(first.id)
  const process1 = /** @type {import('node:child_process').ChildProcess} */ (session.child)
  const [hello] = await once(process1, 'message')
  assert.ok(hello.args.includes(`--data=${root}`))
  assert.ok(hello.args.includes(`--profile=${first.id}`))
  assert.ok(hello.args.some((/** @type {string} */ a) => a.startsWith('--run=')))
  assert.equal(session.ready, true)
  assert.throws(() => session.open(second.id), /先从当前窗口返回/)
  const focused = once(process1, 'message'); session.focus(); assert.equal((await focused)[0].type, 'focused')
  const closed = once(session, 'closed'); process1.send({ type: 'close' })
  assert.equal(session.active, first.id)
  assert.throws(() => session.open(second.id))
  assert.deepEqual((await closed)[0], { id: first.id, error: '' })
  assert.equal(session.active, null)
  session.open(second.id)
  const process2 = /** @type {import('node:child_process').ChildProcess} */ (session.child)
  const [next] = await once(process2, 'message')
  assert.ok(next.args.includes(`--profile=${second.id}`))
  assert.ok(!next.args.includes(`--profile=${first.id}`))
  const gone = once(session, 'closed'); process2.send({ type: 'close' }); await gone
})

test('failed account startup releases the slot and reports the failure', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-session-test-'))
  const store = new ProfileStore(root).load(); const account = store.create('主号', 'Majsoul')
  const session = new ProfileSession({ executable: path.join(root, 'missing'), entry: [], root })
  t.after(() => { assert.ok(root.startsWith(path.join(os.tmpdir(), 'washizu-session-test-'))); fs.rmSync(root, { recursive: true, force: true }) })
  session.open(account.id)
  const [result] = await once(session, 'closed')
  assert.match(result.error, /ENOENT/)
  assert.equal(session.active, null)
})
