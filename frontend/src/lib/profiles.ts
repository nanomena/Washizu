export type AccountProfile = { id: string; name: string; platform: 'Majsoul' | 'Tenhou'; lastOpened: string | null }
export type ProfileClient = { id: string; state: 'starting' | 'active' | 'closing' | 'failed'; cleanup: 'unconfirmed' | 'acknowledged' | 'not-started'; error: string }
export type ProfileState = { revision: number; profiles: AccountProfile[]; clients: ProfileClient[]; error: string }
