import type { Platform, WebPlatform } from '@/lib/platform'

export type GameProxy = { mode: 'system' | 'direct' | 'custom'; server: string }
export type AccountProfile = { id: string; name: string; platform: Platform; lastOpened: string | null; proxy: GameProxy }
export type ProfileClient = { id: string; state: 'starting' | 'active' | 'closing' | 'failed'; cleanup: 'unconfirmed' | 'acknowledged' | 'not-started'; error: string }
export type GameDefaults = { autoplay: 'off' | 'play' | 'continue'; quick_confirm: boolean; pointer_lean: boolean; pointer_trail: boolean; auto_win: boolean; obs_output: boolean }
/** New-account defaults, for the web clients only: a HoloMahjong account has none of these settings. */
export type ProfileDefaults = Record<WebPlatform, GameDefaults>
export type ProfileState = { revision: number; profiles: AccountProfile[]; clients: ProfileClient[]; defaults: ProfileDefaults; error: string }
