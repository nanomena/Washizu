// @ts-check
// One account owns one browser-session proxy, fixed before its first request.
// Core/model/CDP traffic does not use this Chromium session.
/** @typedef {{mode: 'system' | 'direct' | 'custom', server: string}} GameProxy */
export const SYSTEM_PROXY = Object.freeze(/** @type {GameProxy} */ ({ mode: 'system', server: '' }))

/** @param {unknown} value @returns {GameProxy} */
export function validateProxy(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)
    || Object.keys(value).sort().join(',') !== 'mode,server'
    || !('mode' in value) || !['system', 'direct', 'custom'].includes(String(value.mode))
    || !('server' in value) || typeof value.server !== 'string') throw new Error('游戏代理设置无效。')
  const mode = /** @type {GameProxy['mode']} */ (value.mode), raw = value.server.trim()
  if (!raw) {
    if (mode === 'custom') throw new Error('请填写代理地址。')
    return { mode, server: '' }
  }
  let url
  try { url = new URL(raw) } catch { throw new Error('代理地址无效，请填写协议、主机和端口。') }
  if (!['http:', 'https:', 'socks5:'].includes(url.protocol) || !url.hostname || url.port === '0'
    || url.pathname && url.pathname !== '/' || url.search || url.hash || /[\x00-\x20\x7f]/.test(raw)) {
    throw new Error('代理地址须为 HTTP、HTTPS 或 SOCKS5 地址，不包含路径或参数。')
  }
  if (url.username || url.password) throw new Error('暂不支持带账号密码的代理地址，请使用本地代理端口。')
  return { mode, server: `${url.protocol}//${url.host}` }
}

/** @param {GameProxy} value @returns {Electron.ProxyConfig} */
export function proxyConfig(value) {
  const proxy = validateProxy(value)
  if (proxy.mode !== 'custom') return { mode: proxy.mode }
  return { mode: 'fixed_servers', proxyRules: proxy.server, proxyBypassRules: '<local>,127.0.0.0/8,[::1]' }
}

/** The caller awaits this before navigating the game's page. No live switching.
 * @param {Pick<Electron.Session, 'setProxy'>} session @param {GameProxy} proxy */
export async function applyGameProxy(session, proxy) {
  await session.setProxy(proxyConfig(proxy))
}
