// The shell's own record of what it noticed, a line each with the time, in a file beside the core's
// logs: started from a shortcut, the shell has no console for its warnings to reach. Chromium's own
// warnings go to a file of their own beside it, a file each start (see `chromiumSwitches`).

import fs from 'node:fs'
import path from 'node:path'

/** How long the shell's and Chromium's files are kept. */
export const KEEP_DAYS = 14

const two = (/** @type {number} */ n) => String(n).padStart(2, '0')

/** The time as the core's logs write it: the local date and time, to the millisecond. */
export function stamp(date = new Date()) {
  return `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())} ${two(date.getHours())}:${two(date.getMinutes())}:${two(date.getSeconds())}.${String(date.getMilliseconds()).padStart(3, '0')}`
}

/** A start's name for its files, as the core names its run folders: `20261019-130259`. */
export function runName(date = new Date()) {
  return `${date.getFullYear()}${two(date.getMonth() + 1)}${two(date.getDate())}-${two(date.getHours())}${two(date.getMinutes())}${two(date.getSeconds())}`
}

/**
 * The switches that make Chromium write its warnings and errors, from every
 * process, to a file of this start's own. Chromium reads them before the
 * shell's code runs, so the shell starts itself again with them
 * (`app.relaunch`) when they are missing.
 * @param {string} dir @param {string} run
 */
export function chromiumSwitches(dir, run) {
  return ['--enable-logging=file', `--log-file=${path.join(dir, `chromium-${run}.log`)}`, '--log-level=1']
}

/**
 * Whether a start with `argv` is to start again for Chromium's record: not
 * when it carries the switches, nor when it is itself such a second start
 * (`--run`), whatever Chromium made of them — a start never loops.
 * @param {readonly string[]} argv
 */
export function hasChromiumLog(argv) {
  return argv.some(arg => arg.startsWith('--enable-logging') || arg.startsWith('--run='))
}

/**
 * Remove the shell's and Chromium's files older than `days` from `dir`, as of
 * `now`; the core's run folders are the core's.
 * @param {string} dir @param {number} [days] @param {number} [now]
 */
export function prune(dir, days = KEEP_DAYS, now = Date.now()) {
  let names = []
  try { names = fs.readdirSync(dir) } catch { return }
  for (const name of names) {
    if (!/^(shell|chromium)-\d{8}-\d{6}\.log$/.test(name)) continue
    const file = path.join(dir, name)
    try {
      if (now - fs.statSync(file).mtimeMs > days * 24 * 3600 * 1000) fs.unlinkSync(file)
    } catch { /* gone already, or in use: left for the next start */ }
  }
}

/** The shell's record: a line each, with the time, appended to `file` and echoed to the console. */
export class Journal {
  /** @param {string} file */
  constructor(file) {
    this.file = file
  }

  /** @param {string} message */
  note(message) {
    console.warn(message)
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true })
      fs.appendFileSync(this.file, `${stamp()} ${message}\n`)
    } catch { /* a record that cannot be written stops nothing */ }
  }
}
