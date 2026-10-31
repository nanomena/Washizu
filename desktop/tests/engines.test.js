// @ts-check
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { EngineHosts, accountEngine, corePath, defaultEngine, isEngine } from '../src/engines.js'
import { ProfileStore } from '../src/profiles.js'

const STAND_IN = fileURLToPath(new URL('./stand-in-engine-host.js', import.meta.url))
const PACKAGE = 'protocol = "washizu-engine/1"\ndisplay = "Test engine"\nentry = "engine.py"\n'

/** @param {import('node:test').TestContext} t */
function root(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-engines-test-'))
  t.after(() => fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 }))
  return dir
}

/** @param {string} engines @param {string} name */
function install(engines, name) {
  fs.mkdirSync(path.join(engines, name), { recursive: true })
  fs.writeFileSync(path.join(engines, name, 'engine.toml'), PACKAGE)
}

/** @param {() => boolean} ready @param {string} what */
async function until(ready, what) {
  for (let i = 0; i < 200; i++) {
    if (ready()) return
    await new Promise(done => setTimeout(done, 25))
  }
  throw new Error(`timed out: ${what}`)
}

/** @param {string} engines @param {number} [retryMs] */
function hosts(engines, retryMs) {
  /** @type {typeof spawn} */
  const standIn = /** @type {any} */ ((/** @type {string} */ _core, /** @type {string[]} */ args, /** @type {any} */ options) => spawn(process.execPath, [STAND_IN, ...args], options))
  return new EngineHosts({ core: 'akagi-core', engines, spawn: standIn, retryMs })
}

test('only an installed package is an engine, and the default only when installed', t => {
  const engines = path.join(root(t), 'engines')
  install(engines, 'engine-a')
  assert.equal(isEngine(engines, 'engine-a'), true)
  for (const name of ['missing', '', '..', 'a/b', 'a\\b', 'C:x']) assert.equal(isEngine(engines, name), false, name)
  assert.equal(defaultEngine(engines), null)
  fs.writeFileSync(path.join(engines, 'engines.toml'), 'default = "engine-b"\n')
  assert.equal(defaultEngine(engines), null)
  fs.writeFileSync(path.join(engines, 'engines.toml'), '# the shared default\ndefault = "engine-a"\n')
  assert.equal(defaultEngine(engines), 'engine-a')
})

test('an account uses the engine its four-player slot names, else its three-player slot', t => {
  const dir = root(t), engines = path.join(dir, 'engines'), home = path.join(dir, 'home')
  install(engines, 'engine-a')
  fs.mkdirSync(home)
  const write = (/** @type {string} */ text) => fs.writeFileSync(path.join(home, 'config.toml'), text)
  assert.equal(accountEngine(home, engines), null, 'no config')
  write('[bot]\nenabled = true\nactive_4p = "akagi-native"\n')
  assert.equal(accountEngine(home, engines), null, 'a built-in bot is not an engine')
  write('[bot]\nactive_4p = "engine-a" # shared\n\n[monitor]\nactive_3p = "x"\n')
  assert.equal(accountEngine(home, engines), 'engine-a')
  write('[bot]\nactive_4p = "plain-bot"\nactive_3p = "engine-a"\n')
  assert.equal(accountEngine(home, engines), 'engine-a')
  write('[other]\nactive_4p = "engine-a"\n')
  assert.equal(accountEngine(home, engines), null, 'only the [bot] section names bots')
})

test('a new account names the default engine only when one is installed', t => {
  const dir = root(t), engines = path.join(dir, 'engines')
  const store = new ProfileStore(dir).load()
  const plain = store.create('A', 'Tenhou')
  assert.doesNotMatch(fs.readFileSync(path.join(store.home(plain.id), 'config.toml'), 'utf8'), /active_4p/)
  install(engines, 'engine-a')
  fs.writeFileSync(path.join(engines, 'engines.toml'), 'default = "engine-a"\n')
  const shared = store.create('B', 'Tenhou')
  const config = fs.readFileSync(path.join(store.home(shared.id), 'config.toml'), 'utf8')
  assert.match(config, /^\[bot\]\nenabled = true\nactive_4p = "engine-a"\n$/m)
  assert.equal(accountEngine(store.home(shared.id), engines), 'engine-a')
  // Nothing of the engine is copied into the account.
  assert.deepEqual(fs.readdirSync(store.home(shared.id)).sort(), fs.readdirSync(store.home(plain.id)).sort())
})

test('one host serves every account that uses an engine, until the application quits', async t => {
  const engines = path.join(root(t), 'engines')
  install(engines, 'engine-a')
  const owner = hosts(engines)
  t.after(() => owner.stopAll())
  assert.equal(owner.idle, true)
  owner.ensure('engine-a')
  owner.ensure('engine-a')
  const starts = path.join(engines, 'engine-a', 'starts')
  await until(() => fs.existsSync(starts), 'the host started')
  assert.equal(owner.idle, false)
  assert.deepEqual(fs.readFileSync(starts, 'utf8').trim().split('\n'), [`engine-host --engine ${path.join(engines, 'engine-a')}`])
  assert.throws(() => owner.ensure('missing'), /没有安装引擎/)
  await owner.stopAll()
  assert.equal(fs.readFileSync(path.join(engines, 'engine-a', 'stopped'), 'utf8'), 'stopped', 'the host saw its stdin close')
  assert.equal(owner.idle, true)
  assert.throws(() => owner.ensure('engine-a'), /正在退出/)
})

test('a host that stops by itself is started again, and not once the application quits', async t => {
  const engines = path.join(root(t), 'engines')
  install(engines, 'engine-a')
  const crash = path.join(engines, 'engine-a', 'crash')
  fs.writeFileSync(crash, '')
  const owner = hosts(engines, 50)
  t.after(() => owner.stopAll())
  /** @type {string[]} */
  const failures = []
  owner.on('failure', (_name, message) => failures.push(message))
  owner.ensure('engine-a')
  const starts = path.join(engines, 'engine-a', 'starts')
  const count = () => fs.existsSync(starts) ? fs.readFileSync(starts, 'utf8').trim().split('\n').length : 0
  await until(() => count() >= 2, 'started again')
  assert.ok(failures.length >= 1 && failures[0].includes('3'), failures.join('; '))
  fs.rmSync(crash)
  await until(() => !owner.idle && count() >= 3, 'running again')
  await owner.stopAll()
  const stoppedAt = count()
  await new Promise(done => setTimeout(done, 300))
  assert.equal(count(), stoppedAt, 'no host is started after quitting')
})

test('the core is the packaged one unless named', () => {
  assert.equal(corePath({ value: '/x/core', packaged: true, resources: '/r', here: '/h' }), path.resolve('/x/core'))
  assert.match(corePath({ value: '', packaged: true, resources: '/r', here: '/h' }), /core[\\/]akagi-core(\.exe)?$/)
})
