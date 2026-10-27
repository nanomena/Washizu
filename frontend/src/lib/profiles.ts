export type AccountProfile = { id: string; name: string; platform: 'Majsoul' | 'Tenhou'; lastOpened: string | null }
export type ProfileState = { revision: number; profiles: AccountProfile[]; active: string | null; ready: boolean; error: string }
