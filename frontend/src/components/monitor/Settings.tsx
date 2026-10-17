import { useEffect, useState, type ReactNode } from 'react'
import { Check, Copy } from 'lucide-react'
import { AutoplayLevels } from '@/components/monitor/AutoplayLevel'
import { factsOf, levelAbout, levelOf, linkOf, serverLine, type AutoplayLevel, type GameView, type Health, type MonitorConfig, type MonitorSnapshot } from '@/lib/monitor'
import { LANGS, localized, setLang, tr, useLang, type Lang } from '@/lib/i18n'
import { GROUPS, type Group } from '@/lib/settingsGroup'

type Props = {
  snapshot: MonitorSnapshot | null
  options: MonitorConfig
  health: Health[]
  now: number
  /** `null` while the window's own options have not been read, or are being written. */
  onPin: ((pinned: boolean) => void) | null
  onLevel: ((level: AutoplayLevel) => void) | null
  onTrail: ((enabled: boolean) => void) | null
  onLean: ((enabled: boolean) => void) | null
  onStream: ((enabled: boolean) => void) | null
  /** Put the stream's address on the clipboard. */
  onCopyStream: (() => Promise<void>) | null
  onConfirm: ((enabled: boolean) => void) | null
  onAutoWin: ((enabled: boolean) => void) | null
  onGameView: ((view: GameView) => void) | null
  onSpout: ((enabled: boolean) => void) | null
}

const Switch = ({ name, on, onChange }: { name: string; on: boolean; onChange: ((on: boolean) => void) | null }) =>
  <button className="mswitch" role="switch" aria-label={name} aria-checked={on} disabled={!onChange} onClick={() => onChange?.(!on)}>
    <i aria-hidden /><b data-other={on ? tr('关') : tr('开')}>{on ? tr('开') : tr('关')}</b>
  </button>

/** A setting: its name, the line under it (what it does, how it stands, or a value
 *  such as an address; whole on hover), and its control. */
function Row({ name, about, value, control, extra }: { name: string; about: string; value?: boolean; control: ReactNode; extra?: ReactNode }) {
  return <div className={extra ? 'mset-row mset-row--extra' : 'mset-row'}>
    <div><strong>{name}</strong><small data-value={value || undefined} title={about}>{about}</small></div>
    {extra}
    {control}
  </div>
}

/** The stream's address to the clipboard, in a place that is there whether or not there is one. */
function CopyButton({ url, onCopy }: { url: string; onCopy: (() => Promise<void>) | null }) {
  const [copied, setCopied] = useState(false)
  useEffect(() => {
    if (!copied) return
    const done = window.setTimeout(() => setCopied(false), 1500)
    return () => window.clearTimeout(done)
  }, [copied])
  const copy = tr('复制直播地址')
  return <button className="mtool mset-copy" aria-label={copy} title={copied ? tr('已复制') : copy} data-empty={!url || undefined} disabled={!url || !onCopy}
    onClick={() => void onCopy?.().then(() => setCopied(true), () => {})}>
    {copied ? <Check size={14} /> : <Copy size={14} />}
  </button>
}

const Line = ({ name, value, note }: { name: string; value: string; note?: string }) =>
  <div className="mfact" title={note}><dt>{name}</dt><dd>{value}</dd></div>

/** What autoplay does: how far it goes and the two table acts first, then the pointer's. */
function AutoplayRows({ snapshot: s, options, onLevel, onConfirm, onAutoWin, onLean, onTrail }: Props) {
  const sw = (name: string, on: boolean, onChange: ((on: boolean) => void) | null) => <Switch name={name} on={on} onChange={onChange} />
  const level = levelOf(s, options)
  return <>
    <Row name={tr('自动操作')} about={levelAbout(level ?? 'off')} control={<AutoplayLevels level={level} onLevel={onLevel} />} />
    <Row name={tr('快速确认')} about={tr('和了或流局时，等确认按钮出现再点掉结算')} control={sw(tr('快速确认'), !!options.quick_confirm, onConfirm)} />
    <Row name={tr('立直自动和牌')} about={tr('立直后的某个时刻打开雀魂的自动和牌')} control={sw(tr('立直自动和牌'), !!options.riichi_auto_win, onAutoWin)} />
    <Row name={tr('预推理')} about={tr('别家行动时，光标停在下一手可能要打的牌上')} control={sw(tr('预推理'), !!options.pointer_lean, onLean)} />
    <Row name={tr('光标轨迹')} about={tr('在游戏画面上画出自动操作的光标和点击')} control={sw(tr('光标轨迹'), !!options.pointer_trail, onTrail)} />
  </>
}

/** How the game is shown and streamed, then the window itself. The page's renderer
 *  is chosen as it starts, so a change of view waits for the next start. */
function ViewRows({ options, onStream, onCopyStream, onGameView, onSpout, onPin }: Props) {
  const lang = useLang()
  const url = options.stream_output ? options.stream_url ?? '' : ''
  const viewPending = (options.game_view ?? 'window') !== (options.game_view_now ?? 'window')
  const spout = !options.spout_available ? tr('只在 Windows 上可用')
    : options.game_view_now !== 'offscreen' ? tr('先打开离屏渲染，重启后可用')
    : !options.spout_output ? tr('把 2560×1440 的游戏画面交给 OBS 的 Spout2 源')
    : options.spout_state === 'sending' ? tr('OBS 里选择 Spout 发送者「{0}」', options.spout_name ?? 'Akagi')
    : options.spout_state === 'failed' ? tr('发送程序没有运行，正在重试')
    : tr('正在启动发送程序')
  return <>
    {/* Once the stream is on, its line is the address to give the streaming program. */}
    <Row name={tr('直播输出')} about={url || tr('只把游戏画面交给 OBS')} value={!!url} extra={<CopyButton url={url} onCopy={onCopyStream} />}
      control={<Switch name={tr('直播输出')} on={!!options.stream_output} onChange={onStream} />} />
    <Row name={tr('离屏渲染')} about={viewPending ? tr('重启应用后生效') : tr('游戏在窗口外按 2560×1440 绘制')}
      control={<Switch name={tr('离屏渲染')} on={options.game_view === 'offscreen'} onChange={onGameView && (next => onGameView(next ? 'offscreen' : 'window'))} />} />
    <Row name={tr('Spout 输出')} about={spout} control={<Switch name={tr('Spout 输出')} on={!!options.spout_output} onChange={options.spout_available ? onSpout : null} />} />
    <Row name={tr('窗口置顶')} about={tr('让这个窗口保持在其他窗口之上')} control={<Switch name={tr('窗口置顶')} on={options.always_on_top} onChange={onPin} />} />
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
    {row(sync, s?.game && !s.game_ended ? tr('对局 {0} · 局序 {1} · 事件 {2}', s.context.game, s.context.round, s.context.event_seq) : tr('对局开始后跟踪牌局'), tr('当前建议已作废，下一局开始后恢复'))}
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

function AppLines({ snapshot: s, options }: Props) {
  return <>
    <Line name={tr('版本')} value={options.version ?? '—'} />
    <Line name={tr('平台')} value={tr('雀魂')} />
    <Line name={tr('游戏视图')} value={s?.capture.state === 'running' ? s.capture.descriptor : s?.capture.state === 'error' ? tr('异常') : tr('未打开')} />
    <Line name={tr('游戏绘制')} value={options.game_view_now === 'offscreen' ? tr('离屏 2560×1440') : tr('窗口内')} />
  </>
}

/** What the window can be told, and what it runs, a group at a time: the groups in
 *  a list on the left, the chosen one's settings on the right in two columns of
 *  three, each with the line about it. The links are a group of their own, and the
 *  tab carries their one light. */
export function Settings({ group, onGroup, ...props }: Props & { group: Group; onGroup: (group: Group) => void }) {
  const names: Record<Group, string> = { autoplay: tr('自动操作'), view: tr('画面与窗口'), link: tr('连接'), model: tr('模型与服务'), app: tr('应用') }
  return <div className="md-page msets">
    <nav className="mstat mset-groups" aria-label={tr('设置')}>
      {GROUPS.map(key => <button key={key} aria-pressed={key === group} onClick={() => onGroup(key)}>
        {names[key]}{key === 'link' && <i className="mdot" data-tone={linkOf(props.health).tone} />}
      </button>)}
    </nav>
    <section className="mstat">
      <header className="msec-head"><h3>{names[group]}</h3></header>
      {group === 'autoplay' ? <div className="mset-grid"><AutoplayRows {...props} /></div>
        : group === 'view' ? <div className="mset-grid"><ViewRows {...props} /></div>
        : group === 'link' ? <div className="mset-links"><LinkRows {...props} /></div>
        : <dl className="mset-facts">{group === 'model' ? <ModelLines {...props} /> : <AppLines {...props} />}</dl>}
    </section>
  </div>
}

