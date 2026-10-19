// @ts-check
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'
import { Journal, chromiumSwitches, hasChromiumLog, prune, runName, stamp } from '../src/journal.js'
import { FrameMeter, GlWatch, processLine, worthKeeping } from '../src/watch.js'

const AT = new Date(2026, 9, 19, 16, 14, 5, 7)

test('a line has the time as the core writes it, and a start the name of a run folder', () => {
  assert.equal(stamp(AT), '2026-10-19 16:14:05.007')
  assert.equal(runName(AT), '20261019-161405')
})

test("Chromium's record is asked for with switches of the start's own", () => {
  const switches = chromiumSwitches(path.join('d', 'logs'), '20261019-161405')
  assert.deepEqual(switches, ['--enable-logging=file', `--log-file=${path.join('d', 'logs', 'chromium-20261019-161405.log')}`, '--log-level=1'])
  assert.equal(hasChromiumLog(['akagi.exe', '--data=d']), false)
  assert.equal(hasChromiumLog(['akagi.exe', '--data=d', ...switches]), true)
  assert.equal(hasChromiumLog(['akagi.exe', '--data=d', '--run=20261019-161405']), true, 'a second start never starts again')
})

test('the record is appended to, a line at a time', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'journal-'))
  const journal = new Journal(path.join(dir, 'logs', 'shell-1.log'))
  const warn = console.warn
  console.warn = () => {}
  try {
    journal.note('one')
    journal.note('two')
  } finally {
    console.warn = warn
  }
  const lines = fs.readFileSync(path.join(dir, 'logs', 'shell-1.log'), 'utf8').trim().split('\n')
  assert.equal(lines.length, 2)
  assert.match(lines[0], /^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{3} one$/)
  fs.rmSync(dir, { recursive: true })
})

test("only the shell's and Chromium's old files are pruned", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'prune-'))
  const old = Date.now() - 20 * 24 * 3600 * 1000
  for (const name of ['shell-20260901-100000.log', 'chromium-20260901-100000.log', 'shell-20261019-100000.log', 'notes.log']) {
    fs.writeFileSync(path.join(dir, name), 'x')
    if (!name.includes('20261019')) fs.utimesSync(path.join(dir, name), old / 1000, old / 1000)
  }
  fs.mkdirSync(path.join(dir, '20260901-100000'))
  prune(dir)
  assert.deepEqual(fs.readdirSync(dir).sort(), ['20260901-100000', 'notes.log', 'shell-20261019-100000.log'])
  fs.rmSync(dir, { recursive: true })
})

test("the game's console is kept for its warnings, errors and changes of scene", () => {
  assert.equal(worthKeeping('warning', 'Animator.GotoState: State could not be found'), true)
  assert.equal(worthKeeping('error', 'anything'), true)
  assert.equal(worthKeeping('info', 'Unloading 1853 unused Assets to reduce memory usage. Loaded Objects now: 66153.'), true)
  assert.equal(worthKeeping('info', 'WebGL: CONTEXT_LOST_WEBGL: loseContext: context lost'), true)
  assert.equal(worthKeeping('info', 'memorysetup-temp-allocator-size-main=4194304'), false)
})

test('the drawing context is noted when first read and when it is lost or back', () => {
  const gl = new GlWatch()
  assert.equal(gl.seen(null), null, 'nothing to read before the game has its own')
  assert.equal(gl.seen({ lost: false, width: 2560, height: 1440 }), 'game: WebGL context live (2560×1440)')
  assert.equal(gl.seen({ lost: false, width: 2560, height: 1440 }), null)
  assert.equal(gl.seen({ lost: true, width: 0, height: 0 }), 'game: WebGL context lost')
  assert.equal(gl.seen({ lost: true, width: 0, height: 0 }), null)
  assert.equal(gl.seen({ lost: false, width: 2560, height: 1440 }), 'game: WebGL context live again (2560×1440)')
  gl.reset()
  assert.equal(gl.seen({ lost: false, width: 2560, height: 1440 }), 'game: WebGL context live (2560×1440)')
})

test('a page that paints far fewer frames than asked is noted once, and again when it paints', () => {
  const meter = new FrameMeter(60, 10_000)
  const paint = (/** @type {number} */ n) => { for (let i = 0; i < n; i++) meter.paint() }
  paint(600)
  assert.deepEqual(meter.take(), { frames: 600, line: null })
  paint(12)
  assert.deepEqual(meter.take(), { frames: 12, line: 'game: only 12 frames painted in 10 s, of 600 asked' })
  assert.deepEqual(meter.take(), { frames: 0, line: null }, 'once')
  paint(590)
  assert.deepEqual(meter.take(), { frames: 590, line: 'game: painting again, 590 frames in 10 s' })
})

test("the processes are one line, the biggest first", () => {
  const line = processLine([
    { type: 'GPU', pid: 68096, memory: { workingSetSize: 314 * 1024 } },
    { type: 'Tab', pid: 23600, memory: { workingSetSize: 1822 * 1024 } },
  ])
  assert.equal(line, 'Tab 23600 1822 MB, GPU 68096 314 MB')
})
