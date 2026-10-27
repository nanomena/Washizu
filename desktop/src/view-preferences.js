// @ts-check
import fs from 'node:fs'
import path from 'node:path'

/** Copy only the output preference when creating a profile. Nothing about the
 * browser, account, model endpoint or stream secret belongs in this template.
 * @param {string | null} source @returns {boolean} */
export function readObsPreference(source) {
  if (!source) return true
  const current = path.join(source, 'obs.json')
  if (fs.existsSync(current)) {
    const saved = JSON.parse(fs.readFileSync(current, 'utf8'))
    if (!saved || typeof saved.enabled !== 'boolean') throw new Error('OBS 输出设置无法识别。')
    return saved.enabled
  }
  // One-way import of the retired view format; no renderer choice survives.
  const legacy = path.join(source, 'view.json')
  if (!fs.existsSync(legacy)) return true
  const saved = JSON.parse(fs.readFileSync(legacy, 'utf8'))
  if (!saved || typeof saved.spout !== 'boolean') throw new Error('旧版 OBS 输出设置无法识别。')
  return saved.spout
}

/** @param {string} home @param {boolean} enabled */
export function saveObsPreference(home, enabled) {
  const file = path.join(home, 'obs.json')
  const temporary = file + '.tmp'
  fs.writeFileSync(temporary, JSON.stringify({ enabled }))
  fs.renameSync(temporary, file)
}

/** External sender identity stays stable across renaming and concurrent starts.
 * @param {{ id: string, platform: string }} profile */
export function spoutSenderName(profile) {
  return profile.id === 'existing' ? 'Washizu' : `Washizu ${profile.platform} ${profile.id}`
}
