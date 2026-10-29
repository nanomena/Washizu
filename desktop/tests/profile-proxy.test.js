// @ts-check
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { applyGameProxy, proxyConfig, validateProxy } from '../src/profile-proxy.js'

test('custom routing has exactly one upstream, bypasses loopback, and never falls back to direct', async () => {
  const applied = /** @type {Electron.ProxyConfig[]} */ ([])
  await applyGameProxy({ setProxy: async value => { applied.push(value) } }, { mode: 'custom', server: 'http://127.0.0.1:19001' })
  assert.deepEqual(applied, [{ mode: 'fixed_servers', proxyRules: 'http://127.0.0.1:19001', proxyBypassRules: '<local>,127.0.0.0/8,[::1]' }])
  assert.deepEqual(proxyConfig({ mode: 'direct', server: 'http://127.0.0.1:19001' }), { mode: 'direct' })
  assert.deepEqual(proxyConfig({ mode: 'system', server: '' }), { mode: 'system' })
  await assert.rejects(applyGameProxy({ setProxy: async () => { throw new Error('proxy setup failed') } }, { mode: 'custom', server: 'socks5://127.0.0.1:1080' }), /proxy setup failed/)
})

test('malformed proxy addresses and unsupported authentication fail without echoing credentials', () => {
  for (const server of ['', '127.0.0.1:1080', 'ftp://host:80', 'http://host:0', 'http://host:80/path', 'http://host:80?q=a', 'http://host:80#fragment', 'http://secret:password@host:80', 'socks5://secret:password@host:1080']) {
    assert.throws(() => validateProxy({ mode: 'custom', server }), error => error instanceof Error && !/secret|password/.test(error.message))
  }
  assert.deepEqual(validateProxy({ mode: 'custom', server: 'https://EXAMPLE.test:443/' }), { mode: 'custom', server: 'https://example.test' })
  assert.throws(() => validateProxy({ mode: 'system', server: '', fallback: true }), /设置无效/)
})
