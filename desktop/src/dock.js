// @ts-check
// Both the account chooser and the game dock use the same built React app,
// origin and content policy.
import { net, protocol } from 'electron'
import fs from 'node:fs'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { policyFor } from './policy.js'

export function registerDockScheme() {
  protocol.registerSchemesAsPrivileged([{ scheme: 'app', privileges: { standard: true, secure: true, supportFetchAPI: true } }])
}

/** @param {string} directory */
export function serveDock(directory) {
  protocol.handle('app', async request => {
    const url = new URL(request.url)
    const file = path.join(directory, decodeURIComponent(url.pathname === '/' ? '/index.html' : url.pathname))
    const inside = path.relative(directory, file)
    if (url.host !== 'dock' || inside.startsWith('..') || path.isAbsolute(inside)) return new Response(null, { status: 404 })
    try {
      if (file.endsWith('.html')) {
        const html = await fs.promises.readFile(file, 'utf8')
        return new Response(html, { headers: { 'Content-Type': 'text/html; charset=utf-8', 'Content-Security-Policy': policyFor(html) } })
      }
      return await net.fetch(pathToFileURL(file).toString())
    } catch { return new Response(null, { status: 404 }) }
  })
}
