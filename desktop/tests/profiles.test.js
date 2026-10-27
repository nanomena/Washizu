// @ts-check
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'
import { ProfileStore, defaultProfileRoot, homePlatform } from '../src/profiles.js'

/** @param {import('node:test').TestContext} t */
function temporary(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'washizu-profiles-test-'))
  t.after(() => { assert.ok(path.resolve(root).startsWith(path.join(os.tmpdir(), 'washizu-profiles-test-'))); fs.rmSync(root, { recursive: true, force: true }) })
  return root
}

test('adopting and reopening the existing account preserves every original file byte for byte', t => {
  const root = temporary(t)
  const original = {
    'config.toml': '# Keep my unknown settings too\n[platform]\nkind = "Majsoul"\n[bot]\nactive_4p = "miai"\n',
    'browser/Partitions/game/Cookies': 'existing-login-database',
    'browser/Local State': 'existing-encryption-key-reference',
    'browser/Partitions/game/Local Storage/leveldb/000003.log': 'existing-web-storage',
    'history/games/game.jsonl': 'existing-match',
    'mjai_bot/miai/settings.toml': 'port = 7335\nlead_cap = 20000\n',
    'window.json': '{"width":1200}',
  }
  for (const [relative, value] of Object.entries(original)) {
    const file = path.join(root, relative); fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, value)
  }
  const store = new ProfileStore(root).load()
  assert.deepEqual(store.list(), [{ id: 'existing', name: '雀魂 · 原有账号', platform: 'Majsoul', lastOpened: null }])
  assert.equal(store.home('existing'), root)
  const tenhou = store.create('天凤主号', 'Tenhou')
  store.rename('existing', '雀魂主号')
  store.opened('existing')
  const again = new ProfileStore(root).load()
  assert.equal(again.list().length, 2)
  assert.equal(again.get('existing').name, '雀魂主号')
  assert.ok(again.get('existing').lastOpened)
  assert.equal(again.home(tenhou.id), store.home(tenhou.id))
  for (const [relative, value] of Object.entries(original)) assert.equal(fs.readFileSync(path.join(root, relative), 'utf8'), value, relative)
})

test('each platform and each account own a stable independent home', t => {
  const store = new ProfileStore(temporary(t)).load()
  const first = store.create('主号', 'Majsoul')
  const second = store.create('练习号', 'Majsoul')
  const tenhou = store.create('主号', 'Tenhou')
  assert.equal(new Set([first, second, tenhou].map(p => store.home(p.id))).size, 3)
  for (const profile of [first, second, tenhou]) {
    assert.equal(homePlatform(store.home(profile.id)), profile.platform)
    const config = fs.readFileSync(path.join(store.home(profile.id), 'config.toml'), 'utf8')
    assert.match(config, /\[autoplay\]\nenabled = false/)
    assert.match(config, profile.platform === 'Tenhou' ? /https:\/\/tenhou.net\/4\// : /https:\/\/game.maj-soul.com\/1\//)
    assert.equal(fs.existsSync(path.join(store.home(profile.id), 'browser')), false)
    assert.equal(fs.existsSync(path.join(store.home(profile.id), 'mjai_bot')), false)
  }
  const firstHome = store.home(first.id)
  store.rename(first.id, '改过名字的主号')
  assert.equal(store.home(first.id), firstHome)
  assert.throws(() => store.create('主号', 'Tenhou'), /同名/)
  assert.throws(() => store.create(' ', 'Majsoul'), /名称/)
  assert.throws(() => store.create('test', 'Other'), /请选择/)
  assert.throws(() => store.home('../config.toml'), /不存在/)
  store.list()[0].name = 'changed outside the store'
  assert.equal(store.get(first.id).name, '改过名字的主号')
})

test('a damaged registry is reported without replacing it or adopting the home again', t => {
  const root = temporary(t)
  const file = path.join(root, 'profiles.json')
  for (const data of ['not json', '{"version":2,"profiles":[]}', '{"version":1,"profiles":[{"id":"../browser"}]}']) {
    fs.writeFileSync(file, data)
    assert.throws(() => new ProfileStore(root).load())
    assert.equal(fs.readFileSync(file, 'utf8'), data)
  }
})

test('an existing Tenhou home is recognized without touching its config', t => {
  const root = temporary(t)
  const text = '# header\n[platform] # platform\nkind = \'Tenhou\' # comment\n\n[capture]\nurl = "https://tenhou.net/4/"\n'
  fs.writeFileSync(path.join(root, 'config.toml'), text)
  const store = new ProfileStore(root).load()
  assert.equal(store.get('existing').platform, 'Tenhou')
  assert.equal(fs.readFileSync(path.join(root, 'config.toml'), 'utf8'), text)
})

test('production discovery preserves old homes while development stays separate', t => {
  const root = temporary(t)
  const appData = path.join(root, 'appdata')
  const executable = path.join(root, 'desktop', 'Washizu-win32-x64', 'Washizu.exe')
  const old = path.join(appData, 'akagi-desktop')
  fs.mkdirSync(path.join(old, 'browser'), { recursive: true })
  assert.equal(defaultProfileRoot({ appData, executable, packaged: true }), old)
  assert.equal(defaultProfileRoot({ appData, executable, packaged: false }), path.join(appData, 'Washizu-development'))
  const portable = path.join(root, 'desktop', 'data')
  fs.mkdirSync(portable, { recursive: true }); fs.writeFileSync(path.join(portable, 'config.toml'), '')
  assert.equal(defaultProfileRoot({ appData, executable, packaged: true }), portable)
  fs.unlinkSync(path.join(portable, 'config.toml'))
  new ProfileStore(portable).load()
  assert.equal(defaultProfileRoot({ appData, executable, packaged: true }), portable)
})
