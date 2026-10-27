// @ts-check
import test from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

test('first Open shows and focuses the account without a second chooser action', () => {
  const probe = spawnSync(process.execPath, ['--experimental-vm-modules', fileURLToPath(new URL('./obs-output-probe.mjs', import.meta.url))], {
    env: { ...process.env, WINDOW_PRESENTATION_ONLY: '1' }, encoding: 'utf8', timeout: 15000,
  })
  assert.equal(probe.status, 0, `${probe.stdout}\n${probe.stderr}`)
  assert.match(probe.stdout, /First Open, IPC focus and second-instance presentation lifecycle: passed/)
})
