// Actual Rust core engine host + actual Python/uv, with a dependency-free engine
// fixture, owned by the launcher's own EngineHosts. No Electron or game site.
// --core=<built core> --python=<python> --uv=<uv>
import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { EngineHosts } from '../src/engines.js'

/** @param {string} name */
const option = name => process.argv.find(value => value.startsWith(`--${name}=`))?.slice(name.length + 3)
const executable = option('core'), python = option('python'), uv = option('uv')
assert.ok(executable && python && uv, 'pass absolute --core, --python and --uv paths')
for (const file of [executable, python, uv]) assert.ok(path.isAbsolute(file) && fs.existsSync(file), file)
const fixture = fs.readFileSync(fileURLToPath(new URL('../../src/engine/host/fixture.py', import.meta.url)))
/** @type {NodeJS.ProcessEnv} */
const env = { ...process.env, PYTHONDONTWRITEBYTECODE: '1', UV_OFFLINE: 'true' }
const pathKey = Object.keys(env).find(key => key.toLowerCase() === 'path') ?? 'PATH'
// Avoid Windows Store's python3 alias; use the explicitly selected runtime.
env[pathKey] = [path.dirname(python), path.dirname(uv), ...(process.platform === 'win32' ? [path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32')] : ['/usr/bin', '/bin'])].join(path.delimiter)
/** @param {number} ms */
const sleep = ms => new Promise(done => setTimeout(done, ms))
/** @param {() => any} read @param {(value: any) => boolean} match @param {string} label */
async function until(read, match, label) {
  for (let i = 0; i < 600; i++) {
    const value = read()
    if (match(value)) return value
    await sleep(50)
  }
  throw new Error(`timed out: ${label}`)
}
/** @param {number} pid */
const alive = pid => { try { process.kill(pid, 0); return true } catch { return false } }
/** @param {string} file */
const state = file => { try { return JSON.parse(fs.readFileSync(file, 'utf8')).status.state } catch { return null } }

for (const phase of ['ready', 'stubborn']) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-engine-teardown-'))
  const engines = path.join(root, 'engines'), dir = path.join(engines, 'demo')
  fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'engine.toml'), 'protocol = "washizu-engine/1"\ndisplay = "Demo"\nentry = "engine.py"\n')
  fs.writeFileSync(path.join(dir, 'pyproject.toml'), '[project]\nname="teardown-fixture"\nversion="0.0.0"\nrequires-python=">=3.10"\ndependencies=[]\n[tool.uv]\npackage=false\n')
  for (const name of ['engine.py', 'sync']) fs.writeFileSync(path.join(dir, name), fixture)
  fs.writeFileSync(path.join(dir, 'phase'), phase)
  /** @type {typeof spawn} */
  const withEnv = /** @type {any} */ ((/** @type {string} */ command, /** @type {string[]} */ args, /** @type {any} */ options) => spawn(command, args, { ...options, env }))
  /** @type {EngineHosts} */
  const hosts = new EngineHosts({ core: /** @type {string} */ (executable), engines, spawn: withEnv })
  let pid = 0
  try {
    hosts.ensure('demo')
    await until(() => state(path.join(dir, 'host-state.json')), value => value === 'ready', 'actual engine ready through the host')
    pid = Number(fs.readFileSync(path.join(dir, 'service.pid'), 'utf8'))
    const port = Number(fs.readFileSync(path.join(dir, 'port'), 'utf8'))
    assert.ok(alive(pid))
    const began = Date.now()
    await hosts.stopAll()
    assert.equal(hosts.idle, true)
    assert.equal(alive(pid), false, `engine PID ${pid} still alive`)
    assert.equal(fs.existsSync(path.join(dir, 'host-state.json')), false, 'the host withdrew its state')
    await new Promise((done, fail) => {
      const listener = net.createServer().once('error', fail).listen(port, '127.0.0.1', () => listener.close(done))
    })
    console.log(`ok engine host ${phase}: engine PID ${pid} exited, port ${port} reusable, host closed after ${Date.now() - began}ms`)
  } finally {
    await hosts.stopAll()
    if (pid && alive(pid)) { process.kill(pid); await until(() => alive(pid), value => !value, 'fixture cleanup') }
    assert.ok(root.startsWith(path.join(os.tmpdir(), 'washizu-engine-teardown-')))
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 })
  }
}
