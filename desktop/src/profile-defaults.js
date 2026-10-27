// @ts-check
// Only portable UI preferences belong in a new account template. Never copy
// credentials, browser storage, endpoints, model packages or service ports.
import fs from 'node:fs'
import path from 'node:path'
import { readObsPreference } from './view-preferences.js'

/** @typedef {{ autoplay: 'off' | 'play' | 'continue', quick_confirm: boolean, pointer_lean: boolean, pointer_trail: boolean, auto_win: boolean, obs_output: boolean }} GameDefaults */
/** @typedef {{ Majsoul: GameDefaults, Tenhou: GameDefaults }} ProfileDefaults */

/** @param {string | null} home @param {'Majsoul' | 'Tenhou'} platform @returns {GameDefaults} */
export function importDefaults(home, platform) {
  const file = home && path.join(home, 'config.toml')
  const config = file && fs.existsSync(file) ? fs.readFileSync(file, 'utf8') : ''
  /** @param {string} section @param {string} key @param {boolean} fallback */
  const boolean = (section, key, fallback) => {
    const body = config.match(new RegExp(`^\\s*\\[${section}\\][^\\S\\r\\n]*(?:#[^\\r\\n]*)?\\r?\\n([\\s\\S]*?)(?=^\\s*\\[|$(?![\\s\\S]))`, 'm'))?.[1]
    const value = body?.match(new RegExp(`^\\s*${key}\\s*=\\s*(true|false)\\s*(?:#[^\\r\\n]*)?$`, 'm'))?.[1]
    return value === undefined ? fallback : value === 'true'
  }
  return {
    autoplay: !boolean('autoplay', 'enabled', false) ? 'off' : boolean('monitor', 'auto_continue', false) ? 'continue' : 'play',
    quick_confirm: boolean('monitor', 'quick_confirm', true),
    pointer_lean: boolean('monitor', 'pointer_lean', true),
    pointer_trail: boolean('monitor', 'pointer_trail', true),
    auto_win: platform === 'Majsoul' && boolean('monitor', 'auto_win', true),
    obs_output: readObsPreference(home),
  }
}

/** @param {unknown} value @returns {ProfileDefaults} */
export function validateDefaults(value) {
  if (!value || typeof value !== 'object') throw new Error('新档案默认设置无法识别。')
  const record = /** @type {Record<string, any>} */ (value)
  for (const platform of ['Majsoul', 'Tenhou']) {
    const settings = record[platform]
    if (!settings || !['off', 'play', 'continue'].includes(settings.autoplay)
      || ['quick_confirm', 'pointer_lean', 'pointer_trail', 'auto_win', 'obs_output'].some(key => typeof settings[key] !== 'boolean')
      || Object.keys(settings).sort().join() !== 'auto_win,autoplay,obs_output,pointer_lean,pointer_trail,quick_confirm'
      || platform === 'Tenhou' && settings.auto_win) throw new Error('新档案默认设置无效。')
  }
  if (Object.keys(record).sort().join() !== 'Majsoul,Tenhou') throw new Error('新档案默认设置无效。')
  return structuredClone(/** @type {ProfileDefaults} */ (value))
}
