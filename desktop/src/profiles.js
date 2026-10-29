// @ts-check
// Account homes belong to the user. Adopting the old home only records its
// address; it never moves, copies, or rewrites its browser, settings or history.
import fs from 'node:fs'
import path from 'node:path'
import { randomUUID } from 'node:crypto'
import { saveObsPreference } from './view-preferences.js'
import { importDefaults, validateDefaults } from './profile-defaults.js'
import { SYSTEM_PROXY, validateProxy } from './profile-proxy.js'

export const PLATFORMS = Object.freeze({
  Majsoul: { name: '雀魂', url: 'https://game.maj-soul.com/1/' },
  Tenhou: { name: '天凤', url: 'https://tenhou.net/3/' },
})
/** @typedef {keyof typeof PLATFORMS} Platform */
/** @typedef {{ id: string, name: string, platform: Platform, lastOpened: string | null, proxy: import('./profile-proxy.js').GameProxy }} AccountProfile */
/** @typedef {{ version: 2, profiles: AccountProfile[], deleted?: AccountProfile[] }} Registry */

/** @param {unknown} value @returns {Platform} */
export function platformKind(value) {
  if (value !== 'Majsoul' && value !== 'Tenhou') throw new Error('请选择雀魂或天凤。')
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

/** Read only the platform declaration of a core-written config. Fail on an
 * unfamiliar declaration, rather than adopting its browser under another platform.
 * @param {string} home @returns {Platform} */
export function homePlatform(home) {
  const file = path.join(home, 'config.toml')
  if (!fs.existsSync(file)) return 'Majsoul'
  const text = fs.readFileSync(file, 'utf8')
  const section = text.match(/^\s*\[platform\][^\S\r\n]*(?:#[^\r\n]*)?\r?\n([\s\S]*?)(?=^\s*\[|$(?![\s\S]))/m)
  if (!section) return 'Majsoul'
  const kind = section[1].match(/^\s*kind\s*=\s*["']([^"']+)["']\s*(?:#[^\r\n]*)?$/m)
  if (!kind) throw new Error('现有配置的 platform.kind 无法识别，资料未作更改。')
  return platformKind(kind[1])
}

/** @param {string} root */
function hasExistingHome(root) {
  return ['config.toml', 'browser', 'history', 'mjai_bot', 'profiles.json'].some(name => fs.existsSync(path.join(root, name)))
}

/** A new account has no other account's credentials or model service settings.
 * @param {Platform} platform @param {import('./profile-defaults.js').GameDefaults} defaults */
export function initialConfig(platform, defaults) {
  const info = PLATFORMS[platformKind(platform)]
  return `[platform]\nkind = "${platform}"\n\n[capture]\nurl = "${info.url}"\n\n[autoplay]\nenabled = ${defaults.autoplay !== 'off'}\n\n[bot]\nenabled = true\n\n[monitor]\nquick_confirm = ${defaults.quick_confirm}\npointer_lean = ${defaults.pointer_lean}\npointer_trail = ${defaults.pointer_trail}\nauto_win = ${defaults.auto_win}\nauto_continue = ${defaults.autoplay === 'continue'}\n`
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
      const original = this.list().find(p => p.id === 'existing')
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

  /** @param {unknown} name @param {unknown} kind */
  create(name, kind) {
    const platform = platformKind(kind)
    const label = profileName(name)
    this.checkName(label, platform)
    const defaults = this.defaults()[platform]
    const profile = { id: randomUUID(), name: label, platform, lastOpened: null, proxy: { ...SYSTEM_PROXY } }
    const home = path.join(this.root, 'profiles', profile.id)
    fs.mkdirSync(path.dirname(home), { recursive: true })
    fs.mkdirSync(home)
    fs.writeFileSync(path.join(home, 'config.toml'), initialConfig(platform, defaults), { flag: 'wx' })
    saveObsPreference(home, defaults.obs_output)
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
