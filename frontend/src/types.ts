// The shapes the core sends, as far as the monitor page reads them. They mirror
// the Rust side: `src/schema`, `src/game_state` and `src/bot`.

export type MjaiEvent =
  | { type: 'interrupted_kyoku'; reason: string }
  | { type: 'start_game'; names: string[]; kyoku_first?: number; aka_flag?: boolean; id?: number; num_players?: number }
  | { type: 'start_kyoku'; bakaze: string; dora_marker: string; kyoku: number; honba: number; kyotaku: number; oya: number; scores: number[]; tehais: string[][]; num_players?: number }
  | { type: 'tsumo'; actor: number; pai: string }
  | { type: 'dahai'; actor: number; pai: string; tsumogiri: boolean }
  | { type: 'chi'; actor: number; target: number; pai: string; consumed: [string, string] }
  | { type: 'pon'; actor: number; target: number; pai: string; consumed: [string, string] }
  | { type: 'daiminkan'; actor: number; target: number; pai: string; consumed: [string, string, string] }
  | { type: 'kakan'; actor: number; pai: string; consumed: [string, string, string] }
  | { type: 'ankan'; actor: number; consumed: [string, string, string, string] }
  | { type: 'dora'; dora_marker: string }
  | { type: 'reach'; actor: number; pai?: string; tsumogiri?: boolean }
  | { type: 'reach_accepted'; actor: number }
  | { type: 'hora'; actor: number; target: number; deltas?: number[]; ura_markers?: string[]; pai?: string; tehai?: string[]; scores?: number[] }
  | { type: 'ryukyoku'; deltas?: number[]; tehais?: (string[] | null)[]; scores?: number[]; reason?: string }
  | { type: 'kita'; actor: number; pai?: string }
  | { type: 'end_kyoku' }
  | { type: 'end_game' }
  | { type: 'none' }

export type BotResponse = MjaiEvent & { meta?: Record<string, unknown> }

/** Bot-driven custom display payload, attached on `meta.show`.
 *  Schema is intentionally generic so a single tile can render top-N
 *  actions, opponent reads, yaku breakdowns, etc. — bots decide the
 *  semantics and formatting. */
export type ShowItem = {
  /** Primary text on the row. */
  label?: string
  /** mjai tile strings; converted to mahgen via `mjaiToMahgen`. */
  pais?: string[]
  /** Raw mahgen DSL string. Wins over `pais` if both are set. */
  tiles?: string
  /** Right-side text (any format — e.g. "85.42%", "+12000"). */
  value?: string
  /** A discard's `tsumogiri`, as its MJAI event would carry it. Throwing the
   *  tile just drawn and throwing a copy of it from the hand are two candidates;
   *  a bot that names this tells them apart. */
  tsumogiri?: boolean
  /** Hex accent color (e.g. "#00ff80") — applied as left bar + faint row tint. */
  color?: string
  /** Small subtitle under `label`. */
  note?: string
}

export type ShowMeta = {
  /** Optional title; falls back to the tile's default title. */
  title?: string
  /** What the values on the rows are, in a word or two ("Win rate"). */
  caption?: string
  /** A sentence about them, shown on demand. */
  hint?: string
  /** Where this decision falls in the game, by the bot's own count and in its
   *  own words ("step 187"). The monitor numbers the decision by it. */
  position?: string
  items: ShowItem[]
}

export type BotStatus =
  | { state: 'idle' }
  | { state: 'loading'; bot: string; stage: 'syncing_deps' | 'spawning' }
  | { state: 'ready'; bot: string; actor_id: number }
  | { state: 'error'; bot: string; error: string }
  | { state: 'stopped'; bot: string }

export type CaptureStatus =
  | { state: 'stopped' }
  | { state: 'running'; descriptor: string }
  | { state: 'error'; descriptor?: string; error: string }

export type DiscardEntry = {
  tile: string
  tedashi: boolean
  is_riichi: boolean
  /** Claimed by another player (pon/chi/kan); kept for analysis, hidden in the
   * rendered river. */
  called?: boolean
}

export type MeldSnapshot = {
  kind: 'chi' | 'pon' | 'daiminkan' | 'ankan' | 'kakan'
  tiles: string[]
  from_who: number
  called_tile: string | null
}

export type PlayerSnapshot = {
  seat: number
  tehai: string[]
  melds: MeldSnapshot[]
  river: DiscardEntry[]
  score: number
  riichi_declared: boolean
  riichi_stage: boolean
  double_riichi: boolean
  riichi_declaration_index: number | null
  /** 3p only: north tiles set aside via kita / nukidora. Empty in 4p. */
  kita_tiles: string[]
  /** The just-drawn tile, if any. Populated for the active player only. */
  drawn_tile?: string | null
}

export type GameStateSnapshot = {
  bakaze: 'E' | 'S' | 'W' | 'N'
  kyoku: number
  honba: number
  kyotaku: number
  oya: number
  current_player: number
  turn_count: number
  phase: 'wait_act' | 'wait_response'
  is_done: boolean
  /** 3 (sanma) or 4 (yonma). */
  num_players: number
  /** Length matches num_players. */
  players: PlayerSnapshot[]
  dora_markers: string[]
  our_seat: number | null
}

export type PlayerMahgenView = {
  seat: number
  hand: string
  melds: string[]
  river: string
}

export type MahgenView = {
  /** Length matches num_players. */
  players: PlayerMahgenView[]
  /** 3 (sanma) or 4 (yonma). */
  num_players: number
  dora_indicators: string
}
