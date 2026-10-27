import { useState } from 'react'
import { AutoplayRows, LanguageRow, Row, SettingsLayout, Switch } from './SettingsControls'
import type { GameDefaults, ProfileDefaults } from '@/lib/profiles'
import { tr } from '@/lib/i18n'

export function ProfileSettings({ defaults, onChange }: { defaults: ProfileDefaults; onChange: ((value: ProfileDefaults) => void) | null }) {
  const [group, setGroup] = useState<'app' | keyof ProfileDefaults>('app')
  const platform = group === 'app' ? null : group
  const update = (patch: Partial<GameDefaults>) => { if (platform) onChange?.({ ...defaults, [platform]: { ...defaults[platform], ...patch } }) }
  const selected = platform && defaults[platform]
  return <div className="accounts-settings">
    <SettingsLayout groups={['app', 'Majsoul', 'Tenhou']} group={group} onGroup={setGroup} names={{ app: tr('应用'), Majsoul: tr('雀魂'), Tenhou: tr('天凤') }}>
      <div className="mset-grid">
        {selected && platform ? <>
          <AutoplayRows level={selected.autoplay} options={{ ...selected, platform }}
            onLevel={onChange && (autoplay => update({ autoplay }))} onConfirm={onChange && (quick_confirm => update({ quick_confirm }))}
            onAutoWin={onChange && (auto_win => update({ auto_win }))} onLean={onChange && (pointer_lean => update({ pointer_lean }))} onTrail={onChange && (pointer_trail => update({ pointer_trail }))} />
          <Row name={tr('OBS 输出')} about={tr('Windows：在 OBS 添加 Spout2 源，选择「{0}」', 'Washizu')}
            control={<Switch name={tr('OBS 输出')} on={selected.obs_output} onChange={onChange && (obs_output => update({ obs_output }))} />} />
        </> : <LanguageRow />}
      </div>
    </SettingsLayout>
  </div>
}
