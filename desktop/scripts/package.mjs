// @ts-check
// Assemble the application for the platform this runs on: Electron, the shell,
// the core with its Python runtime beside it, and the built interface.
//
//   node scripts/package.mjs [--core=<file>] [--dock=<dir>] [--runtime=<dir>] [--spout=<file>] [--out=<dir>] [--without-runtime]
//
//   --core      the core's executable        (default: target/<triple>/release, then target/release)
//   --dock      the built interface          (default: frontend/dist)
//   --runtime   holds python/<triple> and uv/<triple>, as scripts/fetch-runtime.sh leaves them (default: runtime)
//   --spout     the Spout sender, Windows only (default: native/spout/build/spout-sender.exe, if built;
//               without it the application has no Spout output)
//   --out       where the application goes   (default: dist/desktop)
//   --without-runtime   leave Python out; the core then needs python3 and uv on PATH for bots that use them
//
// The result is <out>/Washizu-<platform>-<arch>/. It is a folder, not an
// installer: it runs from wherever it is put and keeps its data elsewhere.

import { packager } from '@electron/packager'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const DESKTOP = path.dirname(path.dirname(fileURLToPath(import.meta.url)))
const ROOT = path.dirname(DESKTOP)

/** The Rust target the core is built for, by Node's names for this machine. */
const TRIPLES = /** @type {Record<string, string>} */ ({
  'win32-x64': 'x86_64-pc-windows-msvc',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
})
const triple = TRIPLES[`${process.platform}-${process.arch}`]
if (!triple) throw new Error(`No core is built for ${process.platform}-${process.arch}.`)

/** @param {string} name */
const option = name => process.argv.find(arg => arg.startsWith(`--${name}=`))?.slice(name.length + 3)
const coreName = process.platform === 'win32' ? 'akagi-core.exe' : 'akagi-core'
const core = path.resolve(option('core')
  ?? [path.join(ROOT, 'target', triple, 'release', coreName), path.join(ROOT, 'target', 'release', coreName)].find(file => fs.existsSync(file))
  ?? path.join(ROOT, 'target', triple, 'release', coreName))
const dock = path.resolve(option('dock') ?? path.join(ROOT, 'frontend', 'dist'))
const runtime = process.argv.includes('--without-runtime') ? null : path.resolve(option('runtime') ?? path.join(ROOT, 'runtime'))
const out = path.resolve(option('out') ?? path.join(ROOT, 'dist', 'desktop'))
const spout = process.platform === 'win32' ? path.resolve(option('spout') ?? path.join(DESKTOP, 'native', 'spout', 'build', 'spout-sender.exe')) : null

/** @param {string} what @param {string} where @param {string} hint */
const need = (what, where, hint) => { if (!fs.existsSync(where)) throw new Error(`${what} not found at ${where} — ${hint}`) }
need('The core', core, 'cargo build --release --no-default-features --bin akagi-core')
need('The interface', path.join(dock, 'index.html'), 'npm run build in frontend/')
if (runtime) {
  for (const part of ['python', 'uv']) need(`The ${part} runtime`, path.join(runtime, part, triple), `scripts/fetch-runtime.sh ${triple}, or pass --without-runtime`)
}

// What goes beside the shell in the application's resources, laid out the way
// each side looks for it: the core finds its runtime in its own folder
// (`bot::runtime`), the shell finds `core/` and `dock/` (`src/main.js`).
const stage = path.join(out, 'stage')
fs.rmSync(stage, { recursive: true, force: true })
fs.mkdirSync(path.join(stage, 'core'), { recursive: true })
fs.copyFileSync(core, path.join(stage, 'core', coreName))
fs.chmodSync(path.join(stage, 'core', coreName), 0o755)
fs.cpSync(dock, path.join(stage, 'dock'), { recursive: true })
const withSpout = spout !== null && fs.existsSync(spout)
if (option('spout') && spout) need('The Spout sender', spout, 'native/spout/README.md says how to build it')
if (withSpout) {
  fs.mkdirSync(path.join(stage, 'spout'), { recursive: true })
  fs.copyFileSync(spout, path.join(stage, 'spout', 'spout-sender.exe'))
}
if (runtime) {
  for (const part of ['python', 'uv']) {
    // Kept as they are, links included: a Python tree is full of them.
    fs.cpSync(path.join(runtime, part, triple), path.join(stage, 'core', 'runtime', part, triple), { recursive: true, verbatimSymlinks: true })
  }
}

const { version } = JSON.parse(fs.readFileSync(path.join(DESKTOP, 'package.json'), 'utf8'))
// With no platform or architecture named, the packager builds for this machine's.
const [application] = await packager({
  dir: DESKTOP,
  out,
  name: 'Washizu',
  appVersion: version,
  appBundleId: 'com.washizu.desktop',
  icon: path.join(ROOT, 'icons', 'icon'),
  overwrite: true,
  // The shell is a handful of files with no dependencies of its own to hide.
  asar: false,
  prune: true,
  ignore: [/^\/(tests|scripts|native|node_modules)($|\/)/, /^\/(tsconfig\.json|README\.md|package-lock\.json)$/],
  extraResource: [path.join(stage, 'core'), path.join(stage, 'dock'), ...(withSpout ? [path.join(stage, 'spout')] : [])],
  win32metadata: { ProductName: 'Washizu', FileDescription: 'Washizu', CompanyName: 'Washizu' },
})
fs.rmSync(stage, { recursive: true, force: true })
for (const notice of ['LICENSE.txt', 'NOTICE']) {
  if (fs.existsSync(path.join(ROOT, notice))) fs.copyFileSync(path.join(ROOT, notice), path.join(application, `Akagi-${notice}`))
}
console.log(application)
