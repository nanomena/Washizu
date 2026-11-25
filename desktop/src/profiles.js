// @ts-check
// Account homes belong to the user. Adopting the old home only records its
// address; it never moves, copies, or rewrites its browser, settings or history.
import fs from 'node:fs'
import path from 'node:path'
import { randomUUID } from 'node:crypto'
import { createServer } from 'node:net'
import { saveObsPreference } from './view-preferences.js'
import { importDefaults, validateDefaults } from './profile-defaults.js'
import { SYSTEM_PROXY, validateProxy } from './profile-proxy.js'
import { defaultEngine } from './engines.js'

/**
 * How each platform's game reaches the account's window.
 * - `page`: a web client, drawn offscreen in the shell and captured over DevTools.
 * - `mirror`: a native client in a window of its own. The game area shows that
 *   window's picture, view only; its traffic reaches the core's local proxy at
 *   the account's `[capture] listen`.
 * @typedef {{ name: string, view: 'page', url: string }} PagePlatform
 * @typedef {{ name: string, view: 'mirror', windowTitles: readonly string[] }} MirrorPlatform
 */
/** @type {Readonly<{ Majsoul: PagePlatform, Tenhou: PagePlatform, HoloMahjong: MirrorPlatform }>} */
export const PLATFORMS = Object.freeze({
  Majsoul: { name: '雀魂', view: 'page', url: 'https://game.maj-soul.com/1/' },
  Tenhou: { name: '天凤', view: 'page', url: 'https://tenhou.net/3/' },
  // TODO: confirm against the installed client: the exact title of the game's window.
  HoloMahjong: { name: 'ホロじゃん', view: 'mirror', windowTitles: Object.freeze(['ホロライブ麻雀 -ホロじゃん-']) },
})
/** @typedef {keyof typeof PLATFORMS} Platform */
/** @typedef {{ id: string, name: string, platform: Platform, lastOpened: string | null, proxy: import('./profile-proxy.js').GameProxy }} AccountProfile */
/** @typedef {{ version: 2, profiles: AccountProfile[], deleted?: AccountProfile[] }} Registry */

/** @param {unknown} value @returns {Platform} */
export function platformKind(value) {
  if (value !== 'Majsoul' && value !== 'Tenhou' && value !== 'HoloMahjong') throw new Error('请选择雀魂、天凤或ホロじゃん。')
  return value
}

/** @param {unknown} value */
function profileName(value) {
  if (typeof value !== 'string' || !value.trim() || value.trim().length > 48 || /[\x00-\x1f\x7f]/.test(value)) {
    throw new Error('档案名称需要 1–48 个字符。')
  }
  return value.trim()
}

/** @param {string} id */
function validId(id) {
  return id === 'existing' || /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)
}

/** The body of one `[section]` of a config, or null. @param {string} text @param {string} name */
function configSection(text, name) {
  return text.match(new RegExp(`^\\s*\\[${name}\\][^\\S\\r\\n]*(?:#[^\\r\\n]*)?\\r?\\n([\\s\\S]*?)(?=^\\s*\\[|$(?![\\s\\S]))`, 'm'))?.[1] ?? null
}

/** One quoted string value of a config section's body, or null. @param {string} body @param {string} key */
function configString(body, key) {
  return body.match(new RegExp(`^\\s*${key}\\s*=\\s*["']([^"']*)["']\\s*(?:#[^\\r\\n]*)?$`, 'm'))?.[1] ?? null
}

/** Read only the platform declaration of a core-written config. Fail on an
 * unfamiliar declaration, rather than adopting its browser under another platform.
 * @param {string} home @returns {Platform} */
export function homePlatform(home) {
  const file = path.join(home, 'config.toml')
  if (!fs.existsSync(file)) return 'Majsoul'
  const section = configSection(fs.readFileSync(file, 'utf8'), 'platform')
  if (section === null) return 'Majsoul'
  const kind = configString(section, 'kind')
  if (!kind) throw new Error('现有配置的 platform.kind 无法识别，资料未作更改。')
  return platformKind(kind)
}

/** The loopback ports a native client's proxy port is chosen from: below the
 * Windows dynamic range, where outgoing connections and reserved ranges live. */
export const LISTEN_PORTS = Object.freeze({ first: 20000, last: 39999 })

/** `127.0.0.1:<port>`, as the shell writes it and the core binds it.
 * @param {unknown} value @returns {string} */
export function validListen(value) {
  const port = typeof value === 'string' ? value.match(/^127\.0\.0\.1:([1-9][0-9]{3,4})$/)?.[1] : undefined
  if (!port || Number(port) < 1024 || Number(port) > 65535) throw new Error('本地代理地址需要是 127.0.0.1 上的端口。')
  return /** @type {string} */ (value)
}

/** The local proxy address a native client's account was given when it was
 * created. Read, never rewritten: the address is fixed for the account's life.
 * @param {string} home @returns {string} */
export function homeCaptureListen(home) {
  const file = path.join(home, 'config.toml')
  const section = fs.existsSync(file) ? configSection(fs.readFileSync(file, 'utf8'), 'capture') : null
  const listen = section === null ? null : configString(section, 'listen')
  if (!listen) throw new Error('档案配置缺少 capture.listen，资料未作更改。')
  return validListen(listen)
}

/** Whether nothing listens on the loopback port now. @param {number} port @returns {Promise<boolean>} */
export function loopbackFree(port) {
  return new Promise(done => {
    const probe = createServer().once('error', () => done(false)).listen(port, '127.0.0.1', () => probe.close(() => done(true)))
  })
}

/**
 * A port for a new native-client account: free now, and not given to any other
 * account, open or not. Chosen once; the account keeps it.
 * @param {{ taken: ReadonlySet<number>, free?: (port: number) => Promise<boolean>, random?: () => number, attempts?: number }} options
 * @returns {Promise<string>}
 */
export async function chooseListen({ taken, free = loopbackFree, random = Math.random, attempts = 64 }) {
  const span = LISTEN_PORTS.last - LISTEN_PORTS.first + 1
  for (let i = 0; i < attempts; i++) {
    const port = LISTEN_PORTS.first + Math.floor(random() * span)
    if (!taken.has(port) && await free(port)) return `127.0.0.1:${port}`
  }
  throw new Error('找不到可用的本地代理端口，档案未创建。')
}

/** @param {string} root */
function hasExistingHome(root) {
  return ['config.toml', 'browser', 'history', 'mjai_bot', 'profiles.json'].some(name => fs.existsSync(path.join(root, name)))
}

/** @param {string | null} engine */
const botSection = engine => engine ? `enabled = true\nactive_4p = ${JSON.stringify(engine)}\n` : 'enabled = true\n'

/** A new account has no other account's credentials, model package or
 * endpoint. It names the default engine when one is installed: the engine is
 * shared, so its name is all an account holds of it.
 * @param {Platform} platform @param {import('./profile-defaults.js').GameDefaults} defaults @param {string | null} [engine] */
export function initialConfig(platform, defaults, engine = null) {
  const info = PLATFORMS[platformKind(platform)]
  if (info.view !== 'page') throw new Error(`${info.name} 没有网页客户端。`)
  return `[platform]\nkind = "${platform}"\n\n[capture]\nurl = "${info.url}"\n\n[autoplay]\nenabled = ${defaults.autoplay !== 'off'}\n\n[bot]\n${botSection(engine)}\n[monitor]\nquick_confirm = ${defaults.quick_confirm}\npointer_lean = ${defaults.pointer_lean}\npointer_trail = ${defaults.pointer_trail}\nauto_win = ${defaults.auto_win}\nauto_continue = ${defaults.autoplay === 'continue'}\n`
}

/** A native client's account is observed only: its fixed proxy address, and
 * nothing of autoplay, since the window it plays in is not the shell's to press.
 * @param {Platform} platform @param {string} listen @param {string | null} [engine] */
export function mirrorConfig(platform, listen, engine = null) {
  const info = PLATFORMS[platformKind(platform)]
  if (info.view !== 'mirror') throw new Error(`${info.name} 使用网页客户端。`)
  return `[platform]\nkind = "${platform}"\n\n[capture]\nlisten = "${validListen(listen)}"\n\n[autoplay]\nenabled = false\n\n[bot]\n${botSection(engine)}\n[monitor]\nquick_confirm = false\npointer_lean = false\npointer_trail = false\nauto_win = false\nauto_continue = false\n`
}

export class ProfileStore {
  /** @param {string} root */
  constructor(root) {
    this.root = path.resolve(root)
    this.file = path.join(this.root, 'profiles.json')
    /** @type {Registry} */
    this.registry = { version: 2, profiles: [] }
  }

  /** Called after the launcher's single-instance lock is held. */
  load() {
    fs.mkdirSync(this.root, { recursive: true })
    if (fs.existsSync(this.file)) {
      const document = JSON.parse(fs.readFileSync(this.file, 'utf8'))
      if (![1, 2].includes(document?.version) || !Array.isArray(document.profiles)
        || document.deleted !== undefined && !Array.isArray(document.deleted)) throw new Error('账号档案索引无法识别，资料未作更改。')
      const ids = new Set()
      for (const entry of [...document.profiles, ...(document.deleted ?? [])]) {
        if (!entry || typeof entry.id !== 'string' || !validId(entry.id) || ids.has(entry.id)
          || profileName(entry.name) !== entry.name || platformKind(entry.platform) !== entry.platform
          || !(entry.lastOpened === null || (typeof entry.lastOpened === 'string' && Number.isFinite(Date.parse(entry.lastOpened))))) {
          throw new Error('账号档案索引有无效内容，资料未作更改。')
        }
        ids.add(entry.id)
        if (document.version === 2) {
          const proxy = validateProxy(entry.proxy)
          if (proxy.server !== entry.proxy.server) throw new Error('账号档案的代理地址未规范化，资料未作更改。')
        }
      }
      if (document.version === 1) {
        // Explicit one-time migration: preserve the former system routing and
        // every stable identity/home, including recoverable deleted profiles.
        const migrate = (/** @type {Omit<AccountProfile, 'proxy'>} */ profile) => ({ ...profile, proxy: { ...SYSTEM_PROXY } })
        this.registry = { ...document, version: 2, profiles: document.profiles.map(migrate),
          ...(document.deleted && { deleted: document.deleted.map(migrate) }) }
        this.save()
      } else this.registry = document
    } else {
      if (hasExistingHome(this.root)) {
        const platform = homePlatform(this.root)
        this.registry.profiles.push({ id: 'existing', name: `${PLATFORMS[platform].name} · 原有账号`, platform, lastOpened: null, proxy: { ...SYSTEM_PROXY } })
      }
      this.save()
    }
    return this
  }

  list() { return structuredClone(this.registry.profiles) }

  defaults() {
    const file = path.join(this.root, 'launcher', 'defaults.json')
    if (fs.existsSync(file)) return validateDefaults(JSON.parse(fs.readFileSync(file, 'utf8')))
    /** @param {Platform} platform */
    const source = platform => {
      const entries = this.list().filter(p => p.platform === platform)
      // A native client's account holds no web client's settings to import.
      const original = this.list().find(p => p.id === 'existing' && PLATFORMS[p.platform].view === 'page')
      return entries.length ? this.home(entries[0].id) : original ? this.home(original.id) : null
    }
    return { Majsoul: importDefaults(source('Majsoul'), 'Majsoul'), Tenhou: importDefaults(source('Tenhou'), 'Tenhou') }
  }

  /** @param {unknown} value */
  saveDefaults(value) {
    const settings = validateDefaults(value)
    const file = path.join(this.root, 'launcher', 'defaults.json')
    fs.mkdirSync(path.dirname(file), { recursive: true })
    const temporary = `${file}.${randomUUID()}.tmp`
    try {
      fs.writeFileSync(temporary, JSON.stringify(settings, null, 2) + '\n', { flag: 'wx' })
      fs.renameSync(temporary, file)
    } finally { if (fs.existsSync(temporary)) fs.unlinkSync(temporary) }
  }

  /** Remove the selectable account, retaining its data and identity for recovery.
   * The adopted home is the shared root: it must never be recursively deleted.
   * @param {unknown} id */
  delete(id) {
    const profile = this.get(id)
    const before = this.registry
    this.registry = { ...before, profiles: before.profiles.filter(p => p.id !== id), deleted: [...(before.deleted ?? []), profile] }
    try { this.save() } catch (error) { this.registry = before; throw error }
  }

  /** @param {unknown} id */
  get(id) {
    if (typeof id !== 'string' || !validId(id)) throw new Error('账号档案不存在。')
    const found = this.registry.profiles.find(profile => profile.id === id)
    if (!found) throw new Error('账号档案不存在。')
    return structuredClone(found)
  }

  /** A stable id, never the editable display name, owns the directory.
   * @param {unknown} id */
  home(id) {
    const profile = this.get(id)
    return profile.id === 'existing' ? this.root : path.join(this.root, 'profiles', profile.id)
  }

  /** The proxy ports every native-client account holds, deleted ones included:
   * a deleted account can be restored with its home, and its port with it. */
  listenPorts() {
    const ports = new Set()
    for (const profile of [...this.registry.profiles, ...(this.registry.deleted ?? [])]) {
      if (PLATFORMS[profile.platform].view !== 'mirror') continue
      const home = profile.id === 'existing' ? this.root : path.join(this.root, 'profiles', profile.id)
      ports.add(Number(homeCaptureListen(home).split(':')[1]))
    }
    return ports
  }

  /** A web client's account is made from the new-account defaults; a native
   * client's needs the proxy address it keeps for life (`chooseListen`), and
   * nothing else.
   * @param {unknown} name @param {unknown} kind @param {string | null} [listen] */
  create(name, kind, listen = null) {
    const platform = platformKind(kind)
    const label = profileName(name)
    this.checkName(label, platform)
    const info = PLATFORMS[platform]
    if (info.view === 'mirror' ? listen === null : listen !== null) throw new Error(`${info.name} 档案的本地代理地址不符。`)
    if (listen !== null && this.listenPorts().has(Number(validListen(listen).split(':')[1]))) throw new Error('这个本地代理端口已属于另一份档案。')
    const engine = defaultEngine(path.join(this.root, 'engines'))
    const profile = { id: randomUUID(), name: label, platform, lastOpened: null, proxy: { ...SYSTEM_PROXY } }
    const home = path.join(this.root, 'profiles', profile.id)
    fs.mkdirSync(path.dirname(home), { recursive: true })
    fs.mkdirSync(home)
    if (platform === 'HoloMahjong') {
      fs.writeFileSync(path.join(home, 'config.toml'), mirrorConfig(platform, /** @type {string} */ (listen), engine), { flag: 'wx' })
    } else {
      const defaults = this.defaults()[platform]
      fs.writeFileSync(path.join(home, 'config.toml'), initialConfig(platform, defaults, engine), { flag: 'wx' })
      saveObsPreference(home, defaults.obs_output)
    }
    const before = this.registry.profiles
    this.registry.profiles = [...before, profile]
    try { this.save() } catch (error) { this.registry.profiles = before; throw error }
    return structuredClone(profile)
  }

  /** Name and network settings commit as one atomic profile revision.
   * @param {unknown} id @param {unknown} name @param {unknown} proxy */
  update(id, name, proxy) {
    const old = this.get(id)
    const label = profileName(name)
    this.checkName(label, old.platform, old.id)
    this.replace({ ...old, name: label, proxy: validateProxy(proxy) })
    return this.get(id)
  }

  /** Only registry order changes; account data and client lifecycles stay put.
   * @param {unknown} ids */
  reorder(ids) {
    const before = this.registry.profiles
    if (!Array.isArray(ids) || ids.length !== before.length || new Set(ids).size !== ids.length
      || ids.some(id => typeof id !== 'string' || !before.some(profile => profile.id === id))) {
      throw new Error('账号列表已变化，请重新排序。')
    }
    this.registry.profiles = ids.map(id => this.get(id))
    try { this.save() } catch (error) { this.registry.profiles = before; throw error }
  }

  /** @param {unknown} id */
  opened(id) {
    this.replace({ ...this.get(id), lastOpened: new Date().toISOString() })
  }

  /** @param {string} name @param {Platform} platform @param {string} [except] */
  checkName(name, platform, except) {
    if (this.registry.profiles.some(p => p.id !== except && p.platform === platform && p.name === name)) throw new Error('这个平台已有同名档案，请换个名称。')
  }

  /** @param {AccountProfile} profile */
  replace(profile) {
    const before = this.registry.profiles
    this.registry.profiles = before.map(p => p.id === profile.id ? profile : p)
    try { this.save() } catch (error) { this.registry.profiles = before; throw error }
  }

  save() {
    const temporary = `${this.file}.${randomUUID()}.tmp`
    try {
      fs.writeFileSync(temporary, JSON.stringify(this.registry, null, 2) + '\n', { flag: 'wx', mode: 0o600 })
      fs.renameSync(temporary, this.file)
    } finally {
      if (fs.existsSync(temporary)) fs.unlinkSync(temporary)
    }
  }
}

/** Existing installations are adopted in place; development never discovers
 * a production home. --data remains the explicit deployment override.
 * @param {{ appData: string, executable: string, packaged: boolean }} context */
export function defaultProfileRoot({ appData, executable, packaged }) {
  if (!packaged) return path.join(appData, 'Washizu-development')
  const portable = path.resolve(path.dirname(executable), '..', 'data')
  if (hasExistingHome(portable)) return portable
  const previous = path.join(appData, 'akagi-desktop')
  if (hasExistingHome(previous)) return previous
  return path.join(appData, 'Washizu')
}
