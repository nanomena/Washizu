import type { CSSProperties } from 'react'
import { AlertTriangle, Check, Hourglass, Info, LoaderCircle, Minus, X } from 'lucide-react'
import { Faces } from '@/components/monitor/Tiles'
import { controlLabel, decisionMark, type DecisionControl, type ActionView, type Candidate, type DecisionView, type MonitorDecision, type StepState, type Tone } from '@/lib/monitor'
import { localized, tr } from '@/lib/i18n'

const BADGE_ICON: Record<Tone, typeof Check> = { ok: Check, done: Check, wait: Hourglass, busy: LoaderCircle, bad: AlertTriangle, idle: Minus }
const STEPS = localized(['请求', '建议', '确认'] as const)
const STEP_MARK: Partial<Record<StepState, typeof Check>> = { done: Check, bad: X }

/** Tiles and words for one action. The tiles stand in a well of one size, so
 *  the words begin at the same place whether the action shows one tile, four
 *  or none; `data-faces` picks the tile height that fits the well. */
function ActionLine({ action, size }: { action: ActionView; size: 'hero' | 'versus' }) {
  return <div className={`mact mact--${size}`} data-tone={action.tone} data-faces={action.faces.length}>
    <span className="mact-well"><Faces faces={action.faces} /></span>
    <span className="mact-words">
      <span className="mact-text">
        <strong>{action.verb}</strong>
        {action.object && <span>{action.object}</span>}
      </span>
      {size === 'hero' && <em className="mact-note">{action.note}</em>}
    </span>
  </div>
}

function Badge({ view }: { view: DecisionView }) {
  const Icon = BADGE_ICON[view.tone]
  return <span className="mbadge" data-tone={view.tone} role="status">
    <Icon size={12} strokeWidth={2.6} className={view.phase === 'thinking' ? 'mspin' : undefined} />{view.status}
  </span>
}

type CardProps = {
  control: DecisionControl | null
  view: DecisionView
  decision: MonitorDecision | null
  /** Milliseconds the current request has been running; only read while thinking. */
  elapsed: number
  /** The round is over: whatever is shown is the last decision of that round. */
  past: boolean
}

/** The one thing the window exists for: what the model said, and whether
 *  Mahjong Soul then actually did it. Every state fills the same four rows —
 *  heading, action, progress, note — so nothing around the card ever moves. */
export function DecisionCard({ view, decision: d, elapsed, past, control }: CardProps) {
  const thinking = view.phase === 'thinking'
  const versus = view.phase === 'mismatch' && view.action && view.actual
  return <section className="mdec" data-tone={view.tone} data-phase={view.phase} data-past={past || undefined} aria-live="polite">
    <header className="mdec-top">
      <span className="mdec-kicker">
        {d ? <>{past ? tr('上一决策') : tr('决策')} <b className="mdec-mark">{decisionMark(d)}</b></> : tr('模型建议')}
        {view.trigger && <i>{view.trigger}</i>}
      </span>
      <Badge view={view} />
    </header>

    <div className="mdec-main">
      {versus ? <div className="mvs">
        <div className="mvs-side"><small>{tr('模型建议')}</small><ActionLine action={view.action!} size="versus" /></div>
        <span className="mvs-sep" aria-hidden>≠</span>
        <div className="mvs-side mvs-side--actual"><small>{tr('雀魂实际')}</small><ActionLine action={view.actual!} size="versus" /></div>
      </div>
        : view.action ? <ActionLine action={view.action} size="hero" />
        : thinking ? <div className="mact mact--hero mact--pending">
            <span className="mact-well"><span className="mfaces"><span className="mt mt--skeleton" /></span></span>
            <span className="mact-text"><strong>{tr('推理中')}</strong><span className="mdec-clock">{(elapsed / 1000).toFixed(1)} s</span></span>
          </div>
        : <p className="mdec-empty">{view.message}</p>}
    </div>

    <ol className="mrail" aria-label={tr('决策进度')}>
      {view.steps.map((step, i) => {
        const Mark = STEP_MARK[step]
        return <li key={i} data-step={step}><span className="mrail-dot">{Mark && <Mark size={9} strokeWidth={3.5} />}</span><span className="mrail-name" data-text={STEPS[i]}>{STEPS[i]}</span></li>
      })}
    </ol>

    <p className="mdec-msg" data-bad={view.phase === 'failed' || undefined}>
      {control && <span className="mdec-source" data-source={control.source}
        title={control.primary.state === 'ready' || control.primary.state === 'direct_ready' ? controlLabel(control) : control.primary.reason}>{controlLabel(control)}</span>}
      <span title={view.message}>{d && (view.action || thinking || view.phase === 'failed') ? view.message : ''}</span>
      {d?.reaction_ms != null && <span className="mdec-ms" title={tr('完整的 Bot 往返耗时，立直时包含后续请求')}>{tr('往返')} {d.reaction_ms} ms</span>}
    </p>
  </section>
}

type ListProps = {
  rows: Candidate[]
  /** live: the suggestion stands. done: the game did it. void: the game did something else. */
  state: 'live' | 'done' | 'void'
  /** What to say in the slot when there are no rows. */
  empty: string
  loading: boolean
  /** What the rows' values are, and a sentence about them: the bot's own words. */
  caption?: string
  hint?: string
}

/** Ranked candidates, always in the same five slots. The bar behind a row is
 *  its value on a 0–100% scale when that is a percentage, so rows are comparable with
 *  each other and with the number beside them. */
export function CandidateList({ rows, state, empty, loading, caption, hint }: ListProps) {
  // The game's move may not be among the rows listed.
  const missed = state === 'void' && rows.length > 0 && !rows.some(row => row.taken)
  return <section className="mcands" data-state={state}>
    <header className="msec-head">
      <h3>{tr('候选动作')}{state === 'void' && <span>{missed ? tr('已失效 · 实际动作不在其中') : tr('已失效')}</span>}</h3>
      {caption && <span className="msec-aside" title={hint}>{caption}{hint && <Info size={11} />}</span>}
    </header>
    {rows.length > 0 ? <ol className="mcand-list">
      {rows.map((row, i) => <li key={i} className="mcand" data-tone={row.tone} data-chosen={row.chosen || undefined} data-taken={row.taken || undefined}
        style={{ '--share': row.share == null ? '0%' : `${(row.share * 100).toFixed(1)}%` } as CSSProperties}>
        {/* Five columns, the same in every row: a longer word or a fourth tile moves nothing. */}
        <span className="mcand-rank">{i + 1}</span>
        <span className="mcand-verb">{row.verb}</span>
        <span className="mcand-faces"><Faces faces={row.faces} />{row.note && <small className="mcand-note">{row.note}</small>}</span>
        <span className="mcand-mark">{row.chosen ? (state === 'done' ? tr('已执行') : tr('模型选择')) : row.taken ? tr('雀魂实际') : ''}</span>
        <span className="mcand-val">{row.value}</span>
      </li>)}
    </ol>
      : loading ? <ol className="mcand-list mcand-list--loading" aria-hidden>{[0, 1, 2].map(i => <li key={i} className="mcand mcand--skeleton" />)}</ol>
      : <p className="mslot-empty">{empty}</p>}
  </section>
}
