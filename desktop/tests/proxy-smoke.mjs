// @ts-check
import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-proxy-browser-'))
const env = { ...process.env }; delete env.ELECTRON_RUN_AS_NODE
const fixture = fileURLToPath(new URL('./proxy-browser.mjs', import.meta.url))
const result = spawnSync(createRequire(import.meta.url)('electron'), [fixture, '--no-sandbox', `--data=${root}`], { env, encoding: 'utf8', timeout: 30_000 })
process.stdout.write(result.stdout ?? ''); process.stderr.write(result.stderr ?? '')
if (result.error) throw result.error
try { assert.equal(result.status, 0, 'Actual Chromium proxy qualification failed') }
finally {
  const evidence = process.argv.find(arg => arg.startsWith('--evidence='))?.slice('--evidence='.length)
  if (evidence && fs.existsSync(path.join(root, 'management.png'))) {
    fs.mkdirSync(path.resolve(evidence), { recursive: true })
    fs.copyFileSync(path.join(root, 'management.png'), path.join(path.resolve(evidence), 'management.png'))
  }
  assert.equal(path.dirname(path.resolve(root)), path.resolve(os.tmpdir()))
  assert(path.basename(root).startsWith('washizu-proxy-browser-'))
  fs.rmSync(root, { recursive: true, force: true })
}
