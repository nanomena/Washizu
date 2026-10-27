export type AccountProfile = { id: string; name: string; platform: 'Majsoul' | 'Tenhou'; lastOpened: string | null }
export type ProfileClient = { id: string; state: 'starting' | 'active' | 'closing' | 'failed'; cleanup: 'unconfirmed' | 'acknowledged' | 'not-started'; error: string }
export type GameDefaults = { autoplay: 'off' | 'play' | 'continue'; quick_confirm: boolean; pointer_lean: boolean; pointer_trail: boolean; auto_win: boolean; obs_output: boolean }
export type ProfileDefaults = Record<AccountProfile['platform'], GameDefaults>
export type ProfileState = { revision: number; profiles: AccountProfile[]; clients: ProfileClient[]; defaults: ProfileDefaults; error: string }
