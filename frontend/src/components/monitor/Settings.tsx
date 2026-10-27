import { type ReactNode } from 'react'
import { FolderOpen } from 'lucide-react'
import { AutoplayLevels } from '@/components/monitor/AutoplayLevel'
import { factsOf, forecastView, levelAbout, levelOf, linkOf, serverLine, type AutoplayLevel, type Health, type MonitorConfig, type MonitorSnapshot } from '@/lib/monitor'
import { LANGS, localized, setLang, tr, useLang, type Lang } from '@/lib/i18n'
import { GROUPS, type Group } from '@/lib/settingsGroup'

type Props = {
  snapshot: MonitorSnapshot | null
  options: MonitorConfig
  health: Health[]
  now: number
  /** `null` while the window's own options have not been read, or are being written. */
  onLevel: ((level: AutoplayLevel) => void) | null
  onTrail: ((enabled: boolean) => void) | null
  onLean: ((enabled: boolean) => void) | null
  onObsOutput: ((enabled: boolean) => void) | null
  onConfirm: ((enabled: boolean) => void) | null
  onAutoWin: ((enabled: boolean) => void) | null
  onProfiles: (() => void) | null
}

const Switch = ({ name, on, onChange }: { name: string; on: boolean; onChange: ((on: boolean) => void) | null }) =>
  <button className="mswitch" role="switch" aria-label={name} aria-checked={on} disabled={!onChange} onClick={() => onChange?.(!on)}>
    <i aria-hidden /><b data-other={on ? tr('关') : tr('开')}>{on ? tr('开') : tr('关')}</b>
  </button>

/** A setting's name, current status or description (whole on hover), and control. */
function Row({ name, about, control }: { name: string; about: string; control: ReactNode }) {
  return <div className="mset-row">
    <div><strong>{name}</strong><small title={about}>{about}</small></div>
    {control}
  </div>
}

const Line = ({ name, value, note }: { name: string; value: ReactNode; note?: string }) =>
  <div className="mfact" title={note}><dt>{name}</dt><dd>{value}</dd></div>

/** What autoplay does: how far it goes, the results it confirms, the client's auto-win
 *  it switches, then its pointer. */
function AutoplayRows({ snapshot: s, options, onLevel, onConfirm, onAutoWin, onLean, onTrail }: Props) {
  const sw = (name: string, on: boolean, onChange: ((on: boolean) => void) | null) => <Switch name={name} on={on} onChange={onChange} />
  const level = levelOf(s, options)
  const tenhou = options.platform === 'Tenhou'
  const forecast = forecastView(s?.forecast ?? null, options.pointer_lean, s?.autoplay_enabled, s?.game_ended)
  return <>
    <Row name={tr('自动操作')} about={tenhou ? tr('天凤 Web：首次手动选房，连战沿用上一场实际大厅和规则。') : levelAbout(level ?? 'off')} control={<AutoplayLevels level={level} onLevel={onLevel} />} />
    <Row name={tr('快速确认')} about={tr('和了或流局时，等确认按钮出现再点掉结算')} control={sw(tr('快速确认'), !!options.quick_confirm, onConfirm)} />
    <Row name={tr('客户端自动和牌')}
      about={tenhou ? tr('天凤尚未适配客户端自动和牌开关；模型选择的荣和、自摸仍由公共自动打牌流程执行。') : tr('立直或听牌、不到满贯时，某个时刻打开雀魂的自动和牌；没立直时各场第 4 局和西入后不开')}
      control={tenhou ? <span>{tr('暂未适配')}</span> : sw(tr('客户端自动和牌'), !!options.auto_win, onAutoWin)} />
    <Row name={tr('预推理')} about={`${forecast.label} · ${forecast.detail} · ${tr('别家行动时，光标停在下一手可能要打的牌上')}`} control={sw(tr('预推理'), !!options.pointer_lean, onLean)} />
    <Row name={tr('光标轨迹')} about={tr('在游戏画面上画出自动操作的光标和点击')} control={sw(tr('光标轨迹'), !!options.pointer_trail, onTrail)} />
  </>
}

const OBS_STATE: Record<NonNullable<MonitorConfig['obs_state']>, string> = localized({
  off: '输出已关闭', starting: '正在启动 OBS 输出', sending: '正在向 OBS 发送画面', failed: 'OBS 输出失败', stopped: 'OBS 输出已停止',
})

/** One OBS control; rendering and the local game picture do not depend on it. */
function ViewRows({ options, onObsOutput }: Props) {
  const lang = useLang()
  const status = options.obs_state ? OBS_STATE[options.obs_state] : tr('正在读取 OBS 输出状态')
  const about = options.obs_available === false ? `${status} · ${tr('只在 Windows 上可用')}` : status
  const setup = tr('Windows：在 OBS 添加 Spout2 源，选择「{0}」', options.obs_name ?? 'Washizu')
  return <>
    <Row name={tr('OBS 输出')} about={about}
      control={<Switch name={tr('OBS 输出')} on={!!options.obs_output} onChange={options.obs_available === true ? onObsOutput : null} />} />
    <p className="mset-help" title={setup}>{setup}</p>
    <Row name={tr('界面语言')} about={tr('只改变这个窗口里的文字')}
      control={/* Each language under its own name, whatever language is showing. */
        <select className="mselect" aria-label={tr('界面语言')} value={lang} onChange={e => setLang(e.target.value as Lang)}>
          {LANGS.map(([key, name]) => <option key={key} value={key}>{name}</option>)}
        </select>} />
  </>
}

const ago = (ms: number) => (ms < 60_000 ? tr('{0} 秒前', (ms / 1000).toFixed(ms < 10_000 ? 1 : 0)) : tr('{0} 分钟前', Math.floor(ms / 60_000)))

const LINK_NAME: Record<Health['key'], string> = localized({ capture: '采集', bot: 'Bot 进程', service: '模型服务', sync: '牌局同步' })

/** Capture, Bot process, model service and game-state sync are four separate
 *  things and are reported as such: a ready process does not vouch for the
 *  model behind it, and a quiet table is not a broken link. A line a link: its
 *  light, its name, its state, and what it is doing — or the error it gave,
 *  whole on hover and there to be copied. */
function LinkRows({ snapshot: s, health, now }: Props) {
  const [capture, bot, service, sync] = health
  const row = (h: Health, about: string, hint?: string) => <div key={h.key} className="mset-link" data-tone={h.tone}>
    <i className="mdot" data-tone={h.tone} />
    <strong>{LINK_NAME[h.key]}</strong>
    <b>{h.state}</b>
    {h.detail ? <p className="mset-link-error" title={hint ? `${h.detail}
${hint}` : h.detail}>{h.detail}</p> : <p title={about}>{about}</p>}
  </div>
  return <>
    {row(capture, s && s.capture.state !== 'stopped' ? tr('游戏视图{0}', s.capture.descriptor ? ` · ${s.capture.descriptor}` : '') : tr('没有正在运行的采集'))}
    {row(bot, s && 'bot' in s.bot ? `${s.bot.bot}${s.bot.state === 'ready' ? tr(' · 座位 {0}', s.bot.actor_id + 1) : s.bot.state === 'loading' ? ` · ${s.bot.stage === 'syncing_deps' ? tr('同步依赖') : tr('启动进程')}` : ''}` : tr('进程就绪不代表模型已连接'))}
    {row(service, serverLine(s, now), s?.server.state === 'failed' ? tr('应用会自动重新启动它 · {0}', serverLine(s, now)) : undefined)}
    {row(sync, s?.game && !s.game_ended ? tr('对局 {0} · 局序 {1} · 事件 {2}', s.context.game, s.context.round, s.context.event_seq) : tr('对局开始后跟踪牌局'), tr('自动操作等待模型恢复；确认有效建议后，此故障状态才会消失。'))}
  </>
}

/** The model and its service, as the service tells them (the application runs
 *  both), and what this window measures of them. Twelve places. */
function ModelLines({ snapshot: s, now }: Props) {
  const server = s?.server ?? { state: 'none' as const }
  const bot = s && 'bot' in s.bot ? s.bot.bot : null
  // The model service runs from the app's start; the bot itself joins each game as it starts.
  const waiting = !bot && server.state !== 'none' ? server.bot : null
  const d = s?.decision ?? s?.previous_decision ?? null
  return <>
    <Line name={tr('模型')} value={server.state === 'ready' ? server.label ?? '—' : '—'} />
    {/* Longer than its cell once the server is up: the whole of it is there on hover. */}
    <Line name={tr('模型服务')} value={serverLine(s, now, false)} note={serverLine(s, now, false)} />
    <Line name="Bot" value={bot ?? (waiting ? tr('{0} · 对局开始时连接', waiting) : tr('未启动'))} />
    <Line name={tr('决策往返')} value={d?.reaction_ms != null ? `${d.reaction_ms} ms` : '—'} note={tr('完整的 Bot 往返，立直时含后续请求')} />
    <Line name={tr('最近牌局事件')} value={s?.last_event_ms ? ago(Math.max(0, now - s.last_event_ms)) : tr('尚未收到')} note={tr('没有新事件不等于断线')} />
    <Line name={tr('规则')} value={s?.game ? tr('{0} 人{1}', s.game.num_players, s.red_fives == null ? '' : s.red_fives ? tr(' · 有赤牌') : tr(' · 无赤牌')) : '—'} />
    {/* What the bot says about itself, its latest reply first, then what the service was started with. */}
    {factsOf(s, 6).map(fact => <Line key={fact.name} name={fact.name} value={fact.value} note={fact.note} />)}
  </>
}

function AppLines({ snapshot: s, options, onProfiles }: Props) {
  return <>
    <Line name={tr('版本')} value={options.version ?? '—'} />
    <Line name={tr('应用')} value="Washizu" />
    <Line name={tr('平台')} value={options.platform === 'Tenhou' ? tr('天凤') : tr('雀魂')} />
    <Line name={tr('账号档案')} value={<button className="mbtn mset-account" aria-label={tr('显示账号选择器')} title={tr('显示账号选择器')} disabled={!onProfiles} onClick={() => onProfiles?.()}>
      <FolderOpen size={12} /><span>{options.profile_name ?? tr('打开')}</span>
    </button>} />
    <Line name={tr('游戏视图')} value={s?.capture.state === 'running' ? s.capture.descriptor : s?.capture.state === 'error' ? tr('异常') : tr('未打开')} />
    <Line name={tr('关于')} value={tr('基于 Shinkuan 的 Akagi v3')} note={tr('基于 Shinkuan 的 Akagi v3')} />
    <Line name={tr('许可证')} value="Apache-2.0" note={tr('版权与第三方声明见随附的 LICENSE.txt 和 NOTICE。')} />
  </>
}

/** What the window can be told, and what it runs, a group at a time: the groups in
 *  a list on the left, the chosen one's settings on the right a line each, six in
 *  sight and the rest a scroll away, each with the line about it. The links are a group of their own, and the
 *  tab carries their one light. */
export function Settings({ group, onGroup, ...props }: Props & { group: Group; onGroup: (group: Group) => void }) {
  const names: Record<Group, string> = { autoplay: tr('自动操作'), view: tr('画面与输出'), link: tr('连接'), model: tr('模型与服务'), app: tr('应用') }
  return <div className="md-page msets">
    <nav className="mstat mset-groups" aria-label={tr('设置')}>
      {GROUPS.map(key => <button key={key} aria-pressed={key === group} onClick={() => onGroup(key)}>
        {names[key]}{key === 'link' && <i className="mdot" data-tone={linkOf(props.health).tone} />}
      </button>)}
    </nav>
    {/* No heading: the group's name is the one picked in the list beside it. */}
    <section className="mstat" aria-label={names[group]}>
      {group === 'autoplay' ? <div className="mset-grid"><AutoplayRows {...props} /></div>
        : group === 'view' ? <div className="mset-grid"><ViewRows {...props} /></div>
        : group === 'link' ? <div className="mset-links"><LinkRows {...props} /></div>
        : <dl className="mset-facts">{group === 'model' ? <ModelLines {...props} /> : <AppLines {...props} />}</dl>}
    </section>
  </div>
}

