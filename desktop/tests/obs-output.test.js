// @ts-check
import test from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

test('the account shell has one OBS switch and retains offscreen rendering through GPU loss', () => {
  const probe = spawnSync(process.execPath, ['--experimental-vm-modules', fileURLToPath(new URL('./obs-output-probe.mjs', import.meta.url))], { encoding: 'utf8', timeout: 15000 })
  assert.equal(probe.status, 0, `${probe.stdout}\n${probe.stderr}`)
  assert.match(probe.stdout, /OBS contract, fixed offscreen rendering, GPU recovery and migration: passed/)
})

test('an unavailable OBS sender fails explicitly without changing the offscreen renderer', () => {
  const probe = spawnSync(process.execPath, ['--experimental-vm-modules', fileURLToPath(new URL('./obs-output-probe.mjs', import.meta.url))], {
    env: { ...process.env, OBS_TEST_UNAVAILABLE: '1' }, encoding: 'utf8', timeout: 15000,
  })
  assert.equal(probe.status, 0, `${probe.stdout}\n${probe.stderr}`)
  assert.match(probe.stdout, /Unavailable OBS sender: passed/)
})
