// Actual Rust core + actual Python/uv, with a dependency-free service fixture.
// No Electron or game site. --core=<built core> --python=<python> --uv=<uv>
import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { Core } from '../src/core.js'

/** @param {string} name */
const option = name => process.argv.find(value => value.startsWith(`--${name}=`))?.slice(name.length + 3)
const executable = option('core'), python = option('python'), uv = option('uv')
assert.ok(executable && python && uv, 'pass absolute --core, --python and --uv paths')
for (const file of [executable, python, uv]) assert.ok(path.isAbsolute(file) && fs.existsSync(file), file)
const fixture = fs.readFileSync(fileURLToPath(new URL('../../src/bot/service/fixture.py', import.meta.url)))
/** @type {NodeJS.ProcessEnv} */
const env = { ...process.env, PYTHONDONTWRITEBYTECODE: '1', UV_OFFLINE: 'true' }
const pathKey = Object.keys(env).find(key => key.toLowerCase() === 'path') ?? 'PATH'
// Avoid Windows Store's python3 alias; use the explicitly selected runtime.
env[pathKey] = [path.dirname(python), path.dirname(uv), ...(process.platform === 'win32' ? [path.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32')] : ['/usr/bin', '/bin'])].join(path.delimiter)
/** @param {number} ms */
const sleep = ms => new Promise(done => setTimeout(done, ms))
/** @param {() => Promise<any>} read @param {(value: any) => boolean} match @param {string} label */
async function until(read, match, label) {
  for (let i = 0; i < 200; i++) {
    const value = await read()
    if (match(value)) return value
    await sleep(50)
  }
  throw new Error(`timed out: ${label}`)
}
/** @param {number} pid */
const alive = pid => { try { process.kill(pid, 0); return true } catch { return false } }

for (const phase of ['ready', 'stubborn']) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-core-teardown-'))
  const dir = path.join(root, 'bots', 'demo'); fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'bot.py'), '')
  fs.writeFileSync(path.join(dir, 'manifest.toml'), 'manifest_version=1\n[bot]\nname="demo"\n[service]\nentry="service.py"\n')
  fs.writeFileSync(path.join(dir, 'pyproject.toml'), '[project]\nname="teardown-fixture"\nversion="0.0.0"\nrequires-python=">=3.10"\ndependencies=[]\n[tool.uv]\npackage=false\n')
  fs.writeFileSync(path.join(dir, 'service.py'), fixture)
  fs.writeFileSync(path.join(dir, 'phase'), phase)
  fs.writeFileSync(path.join(root, 'config.toml'), `[platform]\nkind="Majsoul"\n[bot]\nenabled=true\ndir=${JSON.stringify(path.join(root, 'bots'))}\nactive_4p="demo"\nactive_3p=""\n[autoplay]\nenabled=false\n`)
  /** @type {Core} */
  const core = new Core(executable, ['--home', root], { env })
  let pid = 0
  try {
    await core.ready
    await until(() => core.call('get_monitor'), snapshot => snapshot.server.state === 'ready', 'actual Rust service ready')
    pid = Number(fs.readFileSync(path.join(dir, 'service.pid'), 'utf8'))
    const port = Number(fs.readFileSync(path.join(dir, 'port'), 'utf8'))
    assert.ok(alive(pid))
    const began = Date.now()
    let finished = false
    const stopped = core.stop().then(() => { finished = true })
    void stopped.catch(() => {})
    if (phase === 'ready') {
      await until(async () => fs.existsSync(path.join(dir, 'eof')), Boolean, 'service receives EOF')
      assert.equal(finished, false, 'core must not acknowledge EOF as process release')
    }
    await stopped
    assert.equal(core.released, true)
    assert.equal(core.exitCode, 0)
    assert.equal(alive(pid), false, `owned service PID ${pid} still alive`)
    await new Promise((done, fail) => {
      const listener = net.createServer().once('error', fail).listen(port, '127.0.0.1', () => listener.close(done))
    })
    console.log(`ok actual Rust core ${phase}: service PID ${pid} exited, port ${port} reusable, cleanup ACK + core close after ${Date.now() - began}ms`)
  } catch (error) {
    console.error(core.stderr)
    throw error
  } finally {
    if (!core.gone) { core.child.kill(); await sleep(300) }
    if (pid && alive(pid)) { process.kill(pid); await until(async () => alive(pid), value => !value, 'fixture cleanup') }
    assert.ok(root.startsWith(path.join(os.tmpdir(), 'washizu-core-teardown-')))
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 })
  }
}
