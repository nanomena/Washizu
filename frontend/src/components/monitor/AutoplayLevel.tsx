import { AUTOPLAY_LEVELS, levelAbout, type AutoplayLevel } from '@/lib/monitor'
import { tr } from '@/lib/i18n'

/** How far autoplay goes, as one control of three: off, playing the game, or
 *  playing game after game. It shows what the core reports, not what was just
 *  pressed. */
export function AutoplayLevels({ level, onLevel, levels = AUTOPLAY_LEVELS }: { level: AutoplayLevel | null; onLevel: ((level: AutoplayLevel) => void) | null; levels?: readonly AutoplayLevel[] }) {
  const names: Record<AutoplayLevel, string> = { off: tr('关'), play: tr('打牌'), continue: tr('连战') }
  return <span className="mlevel" role="radiogroup" aria-label={tr('自动操作')} data-level={level ?? undefined}>
    {levels.map(key => <button key={key} role="radio" aria-checked={key === level} disabled={!onLevel || level == null}
      title={levelAbout(key)} onClick={() => key !== level && onLevel?.(key)}>{names[key]}</button>)}
  </span>
}
