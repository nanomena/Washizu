// @ts-check
// Account homes belong to the user. Adopting the old home only records its
// address; it never moves, copies, or rewrites its browser, settings or history.
import fs from 'node:fs'
import path from 'node:path'
import { randomUUID } from 'node:crypto'

export const PLATFORMS = Object.freeze({
  Majsoul: { name: '雀魂', url: 'https://game.maj-soul.com/1/' },
  Tenhou: { name: '天凤', url: 'https://tenhou.net/4/' },
})
/** @typedef {keyof typeof PLATFORMS} Platform */
/** @typedef {{ id: string, name: string, platform: Platform, lastOpened: string | null }} AccountProfile */
/** @typedef {{ version: 1, profiles: AccountProfile[] }} Registry */

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

/** A new account has no other account's credentials or bot settings. Keep
 * automatic input off; Tenhou's inference adapter is a separate qualification.
 * @param {Platform} platform */
export function initialConfig(platform) {
  const info = PLATFORMS[platformKind(platform)]
  return `[platform]\nkind = "${platform}"\n\n[capture]\nurl = "${info.url}"\n\n[autoplay]\nenabled = false\n\n[bot]\nenabled = ${platform === 'Majsoul'}\n`
}

export class ProfileStore {
  /** @param {string} root */
  constructor(root) {
    this.root = path.resolve(root)
    this.file = path.join(this.root, 'profiles.json')
    /** @type {Registry} */
    this.registry = { version: 1, profiles: [] }
  }

  /** Called after the launcher's single-instance lock is held. */
  load() {
    fs.mkdirSync(this.root, { recursive: true })
    if (fs.existsSync(this.file)) {
      const document = JSON.parse(fs.readFileSync(this.file, 'utf8'))
      if (document?.version !== 1 || !Array.isArray(document.profiles)) throw new Error('账号档案索引无法识别，资料未作更改。')
      const ids = new Set()
      for (const entry of document.profiles) {
        if (!entry || typeof entry.id !== 'string' || !validId(entry.id) || ids.has(entry.id)
          || profileName(entry.name) !== entry.name || platformKind(entry.platform) !== entry.platform
          || !(entry.lastOpened === null || (typeof entry.lastOpened === 'string' && Number.isFinite(Date.parse(entry.lastOpened))))) {
          throw new Error('账号档案索引有无效内容，资料未作更改。')
        }
        ids.add(entry.id)
      }
      this.registry = document
    } else {
      if (hasExistingHome(this.root)) {
        const platform = homePlatform(this.root)
        this.registry.profiles.push({ id: 'existing', name: `${PLATFORMS[platform].name} · 原有账号`, platform, lastOpened: null })
      }
      this.save()
    }
    return this
  }

  list() { return this.registry.profiles.map(profile => ({ ...profile })) }

  /** @param {unknown} id */
  get(id) {
    if (typeof id !== 'string' || !validId(id)) throw new Error('账号档案不存在。')
    const found = this.registry.profiles.find(profile => profile.id === id)
    if (!found) throw new Error('账号档案不存在。')
    return { ...found }
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
    const profile = { id: randomUUID(), name: label, platform, lastOpened: null }
    const home = path.join(this.root, 'profiles', profile.id)
    fs.mkdirSync(path.dirname(home), { recursive: true })
    fs.mkdirSync(home)
    fs.writeFileSync(path.join(home, 'config.toml'), initialConfig(platform), { flag: 'wx' })
    const before = this.registry.profiles
    this.registry.profiles = [...before, profile]
    try { this.save() } catch (error) { this.registry.profiles = before; throw error }
    return { ...profile }
  }

  /** @param {unknown} id @param {unknown} name */
  rename(id, name) {
    const old = this.get(id)
    const label = profileName(name)
    this.checkName(label, old.platform, old.id)
    this.replace({ ...old, name: label })
    return this.get(id)
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
