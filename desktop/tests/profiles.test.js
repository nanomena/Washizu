// @ts-check
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'
import { ProfileStore, defaultProfileRoot, homePlatform } from '../src/profiles.js'
import { spoutSenderName } from '../src/view-preferences.js'

test('ordering persists every stable profile and rejects stale membership without touching account data', t => {
  const store = new ProfileStore(temporary(t)).load(), a = store.create('A', 'Majsoul'), b = store.create('B', 'Tenhou'), c = store.create('C', 'Tenhou')
  store.update(b.id, b.name, { mode: 'custom', server: 'socks5://127.0.0.1:17811' })
  const entries = new Map(store.list().map(p => [p.id, p]))
  const config = fs.readFileSync(path.join(store.home(b.id), 'config.toml'), 'utf8')
  store.reorder([c.id, a.id, b.id])
  const again = new ProfileStore(store.root).load()
  assert.deepEqual(again.list(), [entries.get(c.id), entries.get(a.id), entries.get(b.id)])
  assert.equal(fs.readFileSync(path.join(store.home(b.id), 'config.toml'), 'utf8'), config)
  const committed = fs.readFileSync(store.file, 'utf8')
  for (const ids of [[a.id, b.id], [a.id, a.id, b.id], [a.id, b.id, 'unknown'], [a.id, b.id, 7], 'invalid']) {
    assert.throws(() => store.reorder(ids), /重新排序/)
    assert.equal(fs.readFileSync(store.file, 'utf8'), committed)
  }
  t.mock.method(store, 'save', () => { throw new Error('write refused') })
  assert.throws(() => store.reorder([a.id, b.id, c.id]), /write refused/)
  assert.deepEqual(store.list(), again.list())
})

test('v1 profiles migrate once to explicit system routing without touching account homes', t => {
  const root = temporary(t), file = path.join(root, 'profiles.json')
  const old = { id: 'existing', name: 'Original', platform: 'Majsoul', lastOpened: null }
  fs.writeFileSync(file, JSON.stringify({ version: 1, profiles: [old] }))
  fs.writeFileSync(path.join(root, 'config.toml'), '[platform]\nkind="Majsoul"\n')
  const store = new ProfileStore(root).load()
  assert.deepEqual(store.get('existing').proxy, { mode: 'system', server: '' })
  assert.equal(JSON.parse(fs.readFileSync(file, 'utf8')).version, 2)
  const canonical = fs.readFileSync(file, 'utf8')
  new ProfileStore(root).load()
  assert.equal(fs.readFileSync(file, 'utf8'), canonical)
  const broken = JSON.parse(canonical); delete broken.profiles[0].proxy
  fs.writeFileSync(file, JSON.stringify(broken))
  assert.throws(() => new ProfileStore(root).load(), /代理设置无效/)
  assert.equal(fs.readFileSync(file, 'utf8'), JSON.stringify(broken))
})

test('each profile saves its proxy and name atomically while a running copy stays unchanged', t => {
  const store = new ProfileStore(temporary(t)).load(), a = store.create('A', 'Majsoul'), b = store.create('B', 'Tenhou')
  const running = store.get(a.id)
  store.update(a.id, 'Renamed', { mode: 'custom', server: ' socks5://127.0.0.1:1080 ' })
  const saved = new ProfileStore(store.root).load()
  assert.deepEqual(saved.get(a.id).proxy, { mode: 'custom', server: 'socks5://127.0.0.1:1080' })
  assert.equal(saved.get(a.id).name, 'Renamed')
  assert.deepEqual(saved.get(b.id), b)
  assert.deepEqual(running.proxy, { mode: 'system', server: '' })
  saved.list()[0].proxy.mode = 'direct'
  assert.equal(saved.get(a.id).proxy.mode, 'custom')
  const before = fs.readFileSync(store.file, 'utf8')
  assert.throws(() => store.update(a.id, 'Do not save', { mode: 'custom', server: 'http://secret:password@host:99' }), /账号密码/)
  assert.equal(fs.readFileSync(store.file, 'utf8'), before)
  const fault = t.mock.method(store, 'save', () => { throw new Error('disk write refused') })
  assert.throws(() => store.update(a.id, 'Do not save', { mode: 'direct', server: '' }), /disk write refused/)
  assert.equal(store.get(a.id).name, 'Renamed')
  assert.equal(store.get(a.id).proxy.mode, 'custom')
  fault.mock.restore()
})

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
  assert.deepEqual(store.list(), [{ id: 'existing', name: '雀魂 · 原有账号', platform: 'Majsoul', lastOpened: null, proxy: { mode: 'system', server: '' } }])
  assert.equal(store.home('existing'), root)
  const tenhou = store.create('天凤主号', 'Tenhou')
  store.update('existing', '雀魂主号', store.get('existing').proxy)
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
    assert.match(config, profile.platform === 'Tenhou' ? /https:\/\/tenhou.net\/3\// : /https:\/\/game.maj-soul.com\/1\//)
    assert.equal(fs.existsSync(path.join(store.home(profile.id), 'browser')), false)
    assert.equal(fs.existsSync(path.join(store.home(profile.id), 'mjai_bot')), false)
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(store.home(profile.id), 'obs.json'), 'utf8')), { enabled: true })
  }
  const firstHome = store.home(first.id)
  store.update(first.id, '改过名字的主号', first.proxy)
  assert.equal(store.home(first.id), firstHome)
  assert.throws(() => store.create('主号', 'Tenhou'), /同名/)
  assert.throws(() => store.create(' ', 'Majsoul'), /名称/)
  assert.throws(() => store.create('test', 'Other'), /请选择/)
  assert.throws(() => store.home('../config.toml'), /不存在/)
  store.list()[0].name = 'changed outside the store'
  assert.equal(store.get(first.id).name, '改过名字的主号')
})

test('new profiles inherit only current original Mahjong Soul OBS preference', t => {
  const root = temporary(t)
  const config = '[platform]\nkind = "Majsoul"\n[monitor]\nstream_token = "private"\nstream_port = 50000\n'
  fs.writeFileSync(path.join(root, 'config.toml'), config)
  const original = '{"game_view":"window","spout":false,"unknownSecret":"private"}'
  fs.writeFileSync(path.join(root, 'view.json'), original)
  const store = new ProfileStore(root).load()
  const first = store.create('web', 'Tenhou')
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(store.home(first.id), 'obs.json'), 'utf8')), { enabled: false })
  fs.writeFileSync(path.join(root, 'obs.json'), '{"enabled":true}')
  const second = store.create('next', 'Majsoul')
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(store.home(second.id), 'obs.json'), 'utf8')), { enabled: true })
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(store.home(first.id), 'obs.json'), 'utf8')), { enabled: false })
  assert.equal(fs.readFileSync(path.join(root, 'view.json'), 'utf8'), original)
  assert.doesNotMatch(fs.readFileSync(path.join(store.home(second.id), 'config.toml'), 'utf8'), /private|50000/)
  assert.equal(fs.readFileSync(path.join(root, 'config.toml'), 'utf8'), config)
  assert.equal(spoutSenderName(store.get('existing')), 'Washizu')
  assert.notEqual(spoutSenderName(first), spoutSenderName(second))
  const sender = spoutSenderName(first)
  store.update(first.id, 'renamed', first.proxy)
  assert.equal(spoutSenderName(store.get(first.id)), sender)
})

test('a damaged registry is reported without replacing it or adopting the home again', t => {
  const root = temporary(t)
  const file = path.join(root, 'profiles.json')
  for (const data of ['not json', '{"version":3,"profiles":[]}', '{"version":1,"profiles":[{"id":"../browser"}]}']) {
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

test('deleting either profile removes only its registration and preserves shared and personal data', t => {
  const root = temporary(t)
  fs.writeFileSync(path.join(root, 'config.toml'), '[platform]\nkind = "Majsoul"\n')
  const store = new ProfileStore(root).load()
  const other = store.create('Other', 'Tenhou')
  const otherHome = store.home(other.id)
  store.delete('existing')
  assert.ok(fs.existsSync(path.join(root, 'config.toml')))
  assert.equal(new ProfileStore(root).load().list().length, 1)
  assert.throws(() => store.get('existing'), /不存在/)
  assert.throws(() => store.delete('../'), /不存在/)
  store.delete(other.id)
  assert.ok(fs.existsSync(path.join(otherHome, 'config.toml')))
  assert.equal(new ProfileStore(root).load().list().length, 0)
  assert.equal(JSON.parse(fs.readFileSync(store.file, 'utf8')).deleted.length, 2)
})

test('new account defaults import portable controls per site, persist edits, and do not alter existing accounts', t => {
  const root = temporary(t)
  const original = '[platform]\nkind = "Majsoul"\n[autoplay]\nenabled = true\n[monitor]\npointer_lean = true\npointer_trail = true\nquick_confirm = true\nauto_continue = false\nauto_win = true\n[bot.api]\nkey = "private"\n'
  fs.writeFileSync(path.join(root, 'config.toml'), original)
  const store = new ProfileStore(root).load()
  const tenhou = store.create('Existing Tenhou', 'Tenhou')
  fs.writeFileSync(path.join(store.home(tenhou.id), 'config.toml'), '[platform]\nkind = "Tenhou"\n[autoplay]\nenabled = true\n[monitor]\nauto_continue = true\n')
  const defaults = store.defaults()
  assert.equal(defaults.Majsoul.autoplay, 'play')
  assert.equal(defaults.Tenhou.autoplay, 'continue')
  assert.equal(defaults.Tenhou.auto_win, false)
  defaults.Tenhou.pointer_trail = false
  defaults.Tenhou.obs_output = false
  store.saveDefaults(defaults)
  defaults.Majsoul.pointer_lean = false
  assert.equal(store.defaults().Majsoul.pointer_lean, true, 'caller mutation is not a saved preference')
  const again = new ProfileStore(root).load()
  const created = again.create('Next Tenhou', 'Tenhou')
  const config = fs.readFileSync(path.join(again.home(created.id), 'config.toml'), 'utf8')
  assert.match(config, /enabled = true/)
  assert.match(config, /auto_continue = true/)
  assert.match(config, /pointer_trail = false/)
  assert.match(config, /pointer_lean = true/)
  assert.doesNotMatch(config, /private|key|port/)
  assert.equal(fs.readFileSync(path.join(root, 'config.toml'), 'utf8'), original)
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(again.home(created.id), 'obs.json'), 'utf8')), { enabled: false })
  assert.throws(() => store.saveDefaults({ ...defaults, Tenhou: { ...defaults.Tenhou, auto_win: true } }), /无效/)
})
