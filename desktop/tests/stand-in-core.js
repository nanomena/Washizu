// @ts-check
// A stand-in for the core that speaks its protocol and nothing more, for the
// tests of the shell. `--mode` picks how it behaves:
//   ok       ready at once; answers `echo`, refuses `refuse`, says `monitor-updated` when asked to `nudge`
//   silent   never says it is ready
//   crash    says it is ready, then exits with status 3 on the first request
//   monitor  answers what the interface asks of a core, with a decision on
//            show: enough for the monitor page to draw itself, tiles included
// Ends when its stdin closes, as the core does.
import { createInterface } from 'node:readline'
import { homePlatform } from '../src/profiles.js'
import fs from 'node:fs'
import path from 'node:path'

const mode = process.argv[process.argv.indexOf('--mode') + 1] ?? 'ok'
const say = (/** @type {unknown} */ message) => process.stdout.write(JSON.stringify(message) + '\n')

/** What the page sees: no game on the table, one answered decision with tiles in it. */
/** @type {Record<string, unknown>} */
const monitor = {}
let capture = /** @type {Record<string, string>} */ ({ state: 'stopped' })
function snapshot() {
  const context = { game: 1, round: 1, event_seq: 10 }
  const now = Date.now()
  return {
    revision: 1, context, names: [], red_fives: true,
    game: null, view: null, round_ended: false, game_ended: !process.argv.includes('--live-game'),
    capture, bot: { state: 'ready', bot: 'stand-in', actor_id: 0 },
    service: 'connected', service_error: null, server: { state: 'none' }, sync_error: null,
    brand: 'Stand-in', facts: [], autoplay_enabled: false,
    previous_decision: null, result: null, previous_result: null, events: [], last_event_ms: null,
    decision: {
      id: 1, number: 1, context, started_ms: now, completed_ms: now, reaction_ms: 120, bot: 'stand-in',
      trigger: { type: 'tsumo', actor: 0, pai: '9m' }, state: 'awaiting', error: null, confirmed_seq: null,
      response: {
        type: 'dahai', actor: 0, pai: '9m', tsumogiri: true,
        meta: { show: { items: [{ label: 'Discard', pais: ['9m'], value: '60.0%' }, { label: 'Discard', pais: ['5pr'], value: '25.0%' }] } },
      },
    },
  }
}

/** @type {Record<string, (params: any) => unknown>} */
const answers = {
  get_monitor: snapshot,
  get_hand_events: () => [],
  get_history: () => [],
  set_monitor_autoplay: () => null,
  open_game: ({ port, target }) => {
    if (!Number.isInteger(port) || typeof target !== 'string' || !target) throw new Error('invalid params')
    capture = { state: 'running', kind: 'chromium', descriptor: 'chromium (embedded)' }
    say({ event: 'monitor-updated', payload: { ...snapshot(), revision: 2 } })
    return null
  },
}

process.stderr.write('stand-in core: starting\n')
const homeAt = process.argv.indexOf('--home')
const platform = homeAt >= 0 ? homePlatform(process.argv[homeAt + 1]) : 'Majsoul'
if (mode !== 'silent') say({ event: 'ready', payload: { brand: 'Stand-in', platform, monitor, version: '0.0.0' } })

createInterface({ input: process.stdin }).on('line', line => {
  // Not everything on the line is ours to understand; the core skips what is not a request.
  let request
  try { request = JSON.parse(line) } catch { return }
  if (mode === 'crash') {
    process.stderr.write('stand-in core: giving up\n')
    process.exit(3)
  }
  if (mode === 'monitor' && Object.hasOwn(answers, request.method)) {
    try { say({ id: request.id, result: answers[request.method](request.params ?? {}) }) } catch (error) { say({ id: request.id, error: String(error instanceof Error ? error.message : error) }) }
  } else if (request.method === 'echo') say({ id: request.id, result: request.params })
  else if (request.method === 'nudge') { say({ event: 'monitor-updated', payload: { sequence: 7 } }); say({ id: request.id, result: null }) }
  else say({ id: request.id, error: `unknown method: ${request.method}` })
}).on('close', () => {
  const finish = () => {
    if (mode !== 'shutdown-missing') say({ event: 'shutdown-complete', payload: { ok: mode !== 'shutdown-failure', error: mode === 'shutdown-failure' ? 'model service could not be reaped' : null } })
    process.stdout.write('', () => process.exit(mode === 'shutdown-failure' ? 1 : 0))
  }
  if (process.argv.includes('--delay-release') && homeAt >= 0) {
    const releaseAt = process.argv.indexOf('--release-delay')
    const delay = releaseAt >= 0 ? Number(process.argv[releaseAt + 1]) : 250
    setTimeout(() => { fs.writeFileSync(path.join(process.argv[homeAt + 1], 'fake-core-released'), 'released'); finish() }, delay)
  } else finish()
})
