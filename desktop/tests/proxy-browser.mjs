// @ts-check
// Actual Chromium sessions and sockets; only local fixtures, no game accounts.
import { app, BrowserWindow, session } from 'electron'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { createConnection, createServer as createTcpServer } from 'node:net'
import { createHash } from 'node:crypto'
import os from 'node:os'
import path from 'node:path'
import { applyGameProxy } from '../src/profile-proxy.js'
import { renderProxySettings } from './proxy-render.mjs'

const root = app.commandLine.getSwitchValue('data')
assert(root && path.dirname(path.resolve(root)) === path.resolve(os.tmpdir()) && path.basename(root).startsWith('washizu-proxy-browser-'))
app.setPath('userData', root)
const servers = /** @type {import('node:http').Server[]} */ ([])
const sockets = new Set(/** @type {import('node:net').Socket[]} */ ([]))
const windows = /** @type {Electron.BrowserWindow[]} */ ([])
const socksListeners = /** @type {import('node:net').Server[]} */ ([])
const socksHosts = /** @type {string[]} */ ([])
const hits = /** @type {{label: string, url: string}[]} */ ([])
/** @param {string} label */
async function fixture(label) {
  const server = createServer((req, res) => {
    hits.push({ label, url: req.url ?? '' })
    res.setHeader('Access-Control-Allow-Origin', '*')
    res.end(label)
  })
  server.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)) })
  server.on('connect', (req, socket, head) => {
    if (req.url !== 'account-fixture.invalid:80') { socket.destroy(); return }
    const upstream = createConnection({ host: '127.0.0.1', port: /** @type {import('node:net').AddressInfo} */ (server.address()).port }, () => {
      socket.write('HTTP/1.1 200 Connection Established\r\n\r\n')
      if (head.length) upstream.write(head)
      socket.pipe(upstream); upstream.pipe(socket)
    })
    sockets.add(upstream); upstream.on('close', () => sockets.delete(upstream))
    upstream.on('error', () => socket.destroy())
  })
  server.on('upgrade', (req, socket) => {
    hits.push({ label, url: req.url ?? '' })
    const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64')
    socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + '\r\n\r\n')
    const text = Buffer.from(label)
    socket.write(Buffer.concat([Buffer.from([0x81, text.length]), text]))
    socket.on('data', () => socket.end())
  })
  await new Promise(done => { server.listen(0, '127.0.0.1', () => done(undefined)) })
  servers.push(server)
  return /** @type {import('node:net').AddressInfo} */ (server.address()).port
}

/** A real no-auth SOCKS5 tunnel, restricted to the local HTTP fixture.
 * @param {number} target */
async function socksFixture(target) {
  const server = createTcpServer(socket => {
    sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('error', () => {})
    let pending = Buffer.alloc(0), greeting = true
    const read = (/** @type {Buffer} */ data) => {
      pending = Buffer.concat([pending, data])
      if (greeting) {
        if (pending.length < 2 + pending[1]) return
        if (pending[0] !== 5 || !pending.subarray(2, 2 + pending[1]).includes(0)) { socket.destroy(); return }
        pending = pending.subarray(2 + pending[1]); greeting = false; socket.write(Buffer.from([5, 0]))
      }
      if (pending.length < 5) return
      if (pending[0] !== 5 || pending[1] !== 1 || pending[3] !== 3) { socket.destroy(); return }
      const length = pending[4], size = 7 + length
      if (pending.length < size) return
      const host = pending.subarray(5, 5 + length).toString(), port = pending.readUInt16BE(5 + length)
      if (host !== 'account-fixture.invalid' || port !== 80) { socket.destroy(); return }
      socksHosts.push(host); socket.off('data', read)
      const extra = pending.subarray(size)
      const upstream = createConnection({ host: '127.0.0.1', port: target }, () => {
        socket.write(Buffer.from([5, 0, 0, 1, 127, 0, 0, 1, target >> 8, target & 255]))
        if (extra.length) upstream.write(extra)
        socket.pipe(upstream); upstream.pipe(socket)
      })
      sockets.add(upstream); upstream.on('close', () => sockets.delete(upstream)); upstream.on('error', () => socket.destroy())
    }
    socket.on('data', read)
  })
  await new Promise(done => { server.listen(0, '127.0.0.1', () => done(undefined)) })
  socksListeners.push(server)
  return /** @type {import('node:net').AddressInfo} */ (server.address()).port
}

async function qualify() {
try {
  const [aPort, bPort, localPort] = await Promise.all([fixture('A'), fixture('B'), fixture('LOCAL')])
  const socksPort = await socksFixture(bPort)
  await app.whenReady()
  const a = session.fromPartition('persist:proxy-a'), b = session.fromPartition('persist:proxy-b')
  await Promise.all([
    applyGameProxy(a, { mode: 'custom', server: `http://127.0.0.1:${aPort}` }),
    applyGameProxy(b, { mode: 'custom', server: `socks5://127.0.0.1:${socksPort}` }),
  ])
  const aWindow = new BrowserWindow({ show: false, webPreferences: { session: a, sandbox: true } })
  const bWindow = new BrowserWindow({ show: false, webPreferences: { session: b, sandbox: true } })
  windows.push(aWindow, bWindow)
  await Promise.all([aWindow.loadURL('http://account-fixture.invalid/a'), bWindow.loadURL('http://account-fixture.invalid/b')])
  assert.equal(await aWindow.webContents.executeJavaScript('document.body.innerText'), 'A')
  assert.equal(await bWindow.webContents.executeJavaScript('document.body.innerText'), 'B')
  const webSocket = `(new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error('WebSocket timeout')),5000);const s=new WebSocket('ws://account-fixture.invalid/socket');s.onmessage=e=>{clearTimeout(timer);resolve(e.data);s.close()};s.onerror=()=>{clearTimeout(timer);reject(new Error('WebSocket failed'))}}))`
  assert.equal(await aWindow.webContents.executeJavaScript(webSocket), 'A')
  assert.equal(await bWindow.webContents.executeJavaScript(webSocket), 'B')
  assert(socksHosts.includes('account-fixture.invalid'), 'SOCKS5 receives the domain for remote DNS')
  assert.equal(await (await a.fetch(`http://127.0.0.1:${localPort}/model`)).text(), 'LOCAL')
  assert.equal(await (await b.fetch(`http://127.0.0.1:${localPort}/capture`)).text(), 'LOCAL')
  assert(hits.some(h => h.label === 'A' && h.url.endsWith('/a')))
  assert(hits.some(h => h.label === 'B' && h.url.endsWith('/b')))
  assert(!hits.some(h => h.label === 'A' && h.url.endsWith('/b') || h.label === 'B' && h.url.endsWith('/a')))
  // A failed selected proxy must not reuse B or fall back to a working direct endpoint.
  const dead = createServer(); await new Promise(done => { dead.listen(0, '127.0.0.1', () => done(undefined)) })
  const deadPort = /** @type {import('node:net').AddressInfo} */ (dead.address()).port
  await new Promise(done => dead.close(done))
  const failed = session.fromPartition('persist:proxy-failed')
  await applyGameProxy(failed, { mode: 'custom', server: `http://127.0.0.1:${deadPort}` })
  const badWindow = new BrowserWindow({ show: false, webPreferences: { session: failed, sandbox: true } }); windows.push(badWindow)
  await assert.rejects(badWindow.loadURL('http://account-fixture.invalid/must-fail'), /ERR_PROXY_CONNECTION_FAILED/)
  assert(!hits.some(h => h.url.includes('must-fail')))
  console.log('PASS independent HTTP/SOCKS5 account HTTP/WebSocket routes, remote SOCKS DNS, loopback bypass and explicit proxy failure; no game traffic')
  await renderProxySettings(root)
} catch (error) {
  console.error(error); process.exitCode = 1
} finally {
  for (const window of windows) window.destroy()
  for (const socket of sockets) socket.destroy()
  await Promise.all(servers.map(server => new Promise(done => server.close(done))))
  await Promise.all(socksListeners.map(server => new Promise(done => server.close(done))))
  app.exit(process.exitCode ? Number(process.exitCode) : 0)
}
}
// Electron becomes ready only after evaluating this module.
void qualify()
