import { tr } from '@/lib/i18n'

// The platforms an account can be for, and what the window offers on each: the
// one table every control asks. A control a platform does not have is not
// drawn at all, rather than drawn disabled or pretending.

export type Platform = 'Majsoul' | 'Tenhou' | 'HoloMahjong'
/** The platforms with a web client the shell draws offscreen and can press. */
export type WebPlatform = Exclude<Platform, 'HoloMahjong'>
/** In the order the chooser lists them. */
export const PLATFORMS: readonly Platform[] = ['Majsoul', 'Tenhou', 'HoloMahjong']
/** The platforms the chooser keeps new-account defaults for: those with anything to default. */
export const DEFAULT_PLATFORMS: readonly WebPlatform[] = ['Majsoul', 'Tenhou']

export type Capabilities = {
  /** How the game reaches the stage: the shell's offscreen page, which takes the user's input, or a mirror of the client's own window, view only. */
  view: 'page' | 'mirror'
  /** Autoplay and everything that presses the game for it: the level control, playing on, result confirmation, the pre-inference pointer and its trail. */
  autoplay: boolean
  /** The client's own auto-win switch: switched by autoplay, shown as not yet adapted, or not there. */
  clientAutoWin: 'switch' | 'unadapted' | 'none'
  /** The game picture sent on to OBS. */
  obsOutput: boolean
  /** The account's game proxy, which the shell routes its game page through. */
  gameProxy: boolean
  /** The local proxy address the client's traffic is sent to, shown read-only. */
  captureListen: boolean
}

export const CAPABILITIES: Readonly<Record<Platform, Capabilities>> = {
  Majsoul: { view: 'page', autoplay: true, clientAutoWin: 'switch', obsOutput: true, gameProxy: true, captureListen: false },
  Tenhou: { view: 'page', autoplay: true, clientAutoWin: 'unadapted', obsOutput: true, gameProxy: true, captureListen: false },
  // A native client: observed only, its window mirrored.
  HoloMahjong: { view: 'mirror', autoplay: false, clientAutoWin: 'none', obsOutput: false, gameProxy: false, captureListen: true },
}

/** What `platform` offers; `null` before the shell has said which platform the
 *  window is for. The shell always says it with the window's options, and
 *  until those are read nothing can be pressed (`Monitor`), so nothing is
 *  hidden for that moment either. */
export function capsOf(platform: Platform | undefined): Capabilities | null {
  return platform ? CAPABILITIES[platform] : null
}

/** The platform's name, as the page shows it. */
export function platformName(platform: Platform): string {
  switch (platform) {
    case 'Majsoul': return tr('雀魂')
    case 'Tenhou': return tr('天凤')
    case 'HoloMahjong': return tr('HoloM')
  }
}
