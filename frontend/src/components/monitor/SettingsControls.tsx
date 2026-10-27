import { type ReactNode } from 'react'
import { AutoplayLevels } from '@/components/monitor/AutoplayLevel'
import { levelAbout, type AutoplayLevel, type MonitorConfig } from '@/lib/monitor'
import { LANGS, setLang, tr, useLang, type Lang } from '@/lib/i18n'

type Props = { options: MonitorConfig; onLevel: ((level: AutoplayLevel) => void) | null; onConfirm: ((on: boolean) => void) | null; onAutoWin: ((on: boolean) => void) | null; onLean: ((on: boolean) => void) | null; onTrail: ((on: boolean) => void) | null }

export const Switch = ({ name, on, onChange }: { name: string; on: boolean; onChange: ((on: boolean) => void) | null }) =>
  <button className="mswitch" role="switch" aria-label={name} aria-checked={on} disabled={!onChange} onClick={() => onChange?.(!on)}>
    <i aria-hidden /><b data-other={on ? tr('关') : tr('开')}>{on ? tr('开') : tr('关')}</b>
  </button>

/** A setting's name, current status or description (whole on hover), and control. */
export function Row({ name, about, control }: { name: string; about: string; control: ReactNode }) {
  return <div className="mset-row">
    <div><strong>{name}</strong><small title={about}>{about}</small></div>
    {control}
  </div>
}

/** What autoplay does: how far it goes, the results it confirms, the client's auto-win
 *  it switches, then its pointer. */
export function AutoplayRows({ level, options, onLevel, onConfirm, onAutoWin, onLean, onTrail }: Pick<Props, 'options' | 'onLevel' | 'onConfirm' | 'onAutoWin' | 'onLean' | 'onTrail'> & { level: AutoplayLevel | null }) {
  const sw = (name: string, on: boolean, onChange: ((on: boolean) => void) | null) => <Switch name={name} on={on} onChange={onChange} />
  const tenhou = options.platform === 'Tenhou'
  return <>
    <Row name={tr('自动操作')} about={tenhou ? tr('天凤 Web：首次手动选房，连战沿用上一场实际大厅和规则。') : levelAbout(level ?? 'off')} control={<AutoplayLevels level={level} onLevel={onLevel} />} />
    <Row name={tr('快速确认')} about={tr('和了或流局时，等确认按钮出现再点掉结算')} control={sw(tr('快速确认'), !!options.quick_confirm, onConfirm)} />
    <Row name={tr('客户端自动和牌')}
      about={tenhou ? tr('天凤尚未适配客户端自动和牌开关；模型选择的荣和、自摸仍由公共自动打牌流程执行。') : tr('立直或听牌、不到满贯时，某个时刻打开雀魂的自动和牌；没立直时各场第 4 局和西入后不开')}
      control={tenhou ? <span>{tr('暂未适配')}</span> : sw(tr('客户端自动和牌'), !!options.auto_win, onAutoWin)} />
    <Row name={tr('预推理')} about={tr('别家行动时，光标停在下一手可能要打的牌上')} control={sw(tr('预推理'), !!options.pointer_lean, onLean)} />
    <Row name={tr('光标轨迹')} about={tr('在游戏画面上画出自动操作的光标和点击')} control={sw(tr('光标轨迹'), !!options.pointer_trail, onTrail)} />
  </>
}

export function LanguageRow() {
  const lang = useLang()
  return <Row name={tr('界面语言')} about={tr('只改变这个窗口里的文字')} control={<select className="mselect" aria-label={tr('界面语言')} value={lang} onChange={event => setLang(event.target.value as Lang)}>{LANGS.map(([key, name]) => <option key={key} value={key}>{name}</option>)}</select>} />
}

export function SettingsLayout<G extends string>({ groups, group, names, onGroup, children, badges }: { groups: readonly G[]; group: G; names: Record<G, string>; onGroup: (group: G) => void; children: ReactNode; badges?: Partial<Record<G, ReactNode>> }) {
  return <div className="md-page msets">
    <nav className="mstat mset-groups" aria-label={tr('设置')}>{groups.map(key => <button key={key} aria-pressed={key === group} onClick={() => onGroup(key)}>{names[key]}{badges?.[key]}</button>)}</nav>
    <section className="mstat" aria-label={names[group]}>{children}</section>
  </div>
}
