//! Mahjong Soul replay → tenhou.net/6 log.
//!
//! When the client opens a replay it asks the lobby for the whole game
//! (`.lq.Lobby.fetchGameRecord`). Unlike the live game feed, that answer
//! carries every seat's starting hand and draws, so it converts to the
//! tenhou.net/6 JSON log that replay viewers read, and to the text NAGA's
//! custom-game analysis takes: one `http://tenhou.net/6/#json=` line per
//! round.
//!
//! The conversion is a port of the userscript in
//! [honvl/Majsoul-to-NAGA](https://github.com/honvl/Majsoul-to-NAGA) (MIT),
//! itself adapted from AsaChiri/majsoul-naga, which was checked round for
//! round against tensoul-py. Where it departs: abortive draws are named from
//! `RecordLiuJu.type` (codes confirmed against the official client, see this
//! bridge's README) with tenhou's own names, instead of being guessed from
//! riichi and kan counts. Only 4-player games convert.

use super::parser::{decode_message, unwrap_message};
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Serialize;
use serde_json::{json, Number, Value as JsonValue};
use std::path::{Path, PathBuf};

/// What NAGA's order form expects in front of each round's JSON.
pub const NAGA_PREFIX: &str = "http://tenhou.net/6/#json=";

/// A tenhou.net/6 log. Field order is the order tenhou's own logs use.
#[derive(Debug, Clone, Serialize)]
pub struct TenhouLog {
    pub ver: &'static str,
    #[serde(rename = "ref")]
    pub reference: String,
    pub ratingc: &'static str,
    pub rule: Rule,
    pub lobby: u32,
    pub dan: [&'static str; 4],
    pub rate: [u32; 4],
    pub sx: [&'static str; 4],
    pub name: [String; 4],
    /// Final points and placement points, two per seat.
    pub sc: Vec<Number>,
    pub title: [String; 2],
    /// One entry per round, in tenhou's array layout.
    pub log: Vec<JsonValue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    pub disp: String,
    pub aka: u32,
}

/// One round as NAGA's order form takes it.
#[derive(Serialize)]
struct NagaRound<'a> {
    title: &'a [String; 2],
    name: &'a [String; 4],
    rule: &'a Rule,
    log: [&'a JsonValue; 1],
}

impl TenhouLog {
    /// NAGA's order-form text: one `http://tenhou.net/6/#json=` line per round.
    pub fn naga_text(&self) -> String {
        let mut out = String::new();
        for round in &self.log {
            let one = NagaRound {
                title: &self.title,
                name: &self.name,
                rule: &self.rule,
                log: [round],
            };
            out.push_str(NAGA_PREFIX);
            out.push_str(&serde_json::to_string(&one).expect("round serializes"));
            out.push('\n');
        }
        out
    }
}

/// Files written for one replay.
#[derive(Debug, Clone)]
pub struct Exported {
    pub uuid: String,
    pub rounds: usize,
    pub log: PathBuf,
    pub naga: PathBuf,
}

/// Convert a `fetchGameRecord` response and write it under `dir` as
/// `<uuid>.json` (the tenhou.net/6 log) and `<uuid>.naga.txt`.
pub fn export(dir: &Path, response: &JsonValue) -> Result<Exported> {
    let log = from_record(response)?;
    let stem = file_stem(&log.reference);
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let log_path = dir.join(format!("{stem}.json"));
    let naga_path = dir.join(format!("{stem}.naga.txt"));
    std::fs::write(&log_path, serde_json::to_string(&log)?)
        .with_context(|| format!("write {}", log_path.display()))?;
    std::fs::write(&naga_path, log.naga_text())
        .with_context(|| format!("write {}", naga_path.display()))?;
    Ok(Exported {
        uuid: log.reference.clone(),
        rounds: log.log.len(),
        log: log_path,
        naga: naga_path,
    })
}

fn file_stem(uuid: &str) -> String {
    let stem: String = uuid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if stem.is_empty() {
        "replay".into()
    } else {
        stem
    }
}

/// Convert a decoded `ResGameRecord` (the `fetchGameRecord` response payload,
/// bytes fields as base64) into a tenhou.net/6 log.
pub fn from_record(response: &JsonValue) -> Result<TenhouLog> {
    let code = int(response.get("error").unwrap_or(&JsonValue::Null), "code");
    ensure!(code == 0, "fetchGameRecord answered error {code}");
    let head = response.get("head").context("record has no head")?;
    let data = text(response, "data");
    if data.is_empty() {
        // The error reaches the shareable log; a query could be a signature.
        let url = crate::util::redact_query(text(response, "data_url"));
        bail!("record is not inline (data_url {url:?}); open it again to fetch it");
    }
    let blob = BASE64.decode(data).context("record data is not base64")?;
    let (name, inner) = unwrap_message(&blob)?;
    ensure!(
        name.trim_start_matches('.') == "lq.GameDetailRecords",
        "record data is {name}, not GameDetailRecords"
    );
    let details = decode_message(&name, &inner)?;

    // Records from before 2021-07-15 list wrappers directly; later ones wrap
    // each in a GameAction whose `result` holds it.
    let blobs: Vec<&str> =
        if int(&details, "version") < 210715 && !list(&details, "records").is_empty() {
            list(&details, "records")
                .iter()
                .filter_map(JsonValue::as_str)
                .collect()
        } else {
            list(&details, "actions")
                .iter()
                .map(|a| text(a, "result"))
                .filter(|r| !r.is_empty())
                .collect()
        };
    let mut game = Game::default();
    for b64 in blobs {
        let bytes = BASE64.decode(b64).context("record entry is not base64")?;
        let (name, inner) = unwrap_message(&bytes)?;
        let short = name.trim_start_matches(".lq.");
        // other records (3-player kita, event modes) carry nothing tenhou shows
        if RECORDS.contains(&short) {
            game.feed(short, &decode_message(&name, &inner)?)?;
        }
    }
    ensure!(!game.kyokus.is_empty(), "record has no rounds");

    let mut names: [String; 4] = std::array::from_fn(|_| "AI".to_string());
    for account in list(head, "accounts") {
        let seat = int(account, "seat");
        let nick = text(account, "nickname");
        if (0..4).contains(&seat) && !nick.is_empty() {
            names[seat as usize] = nick.to_string();
        }
    }
    let mut sc: Vec<Number> = vec![Number::from(0); 8];
    for player in list(head.get("result").unwrap_or(&JsonValue::Null), "players") {
        let seat = int(player, "seat");
        if (0..4).contains(&seat) {
            let s = seat as usize;
            sc[2 * s] = Number::from(int(player, "part_point_1"));
            sc[2 * s + 1] = thousandths(int(player, "total_point"));
        }
    }
    let mode = head
        .get("config")
        .and_then(|c| c.get("mode"))
        .map(|m| int(m, "mode"))
        .unwrap_or(0);
    // display only: 1 is an east-only game, anything else a south game
    let disp = format!("{}喰赤", if mode == 1 { "東" } else { "南" });
    let end_time = int(head, "end_time");
    let when = chrono::DateTime::from_timestamp(end_time, 0)
        .filter(|_| end_time > 0)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    Ok(TenhouLog {
        ver: "2.3",
        reference: text(head, "uuid").to_string(),
        ratingc: "PF4",
        rule: Rule {
            disp: disp.clone(),
            aka: 1,
        },
        lobby: 0,
        dan: ["", "", "", ""],
        rate: [0, 0, 0, 0],
        sx: ["C", "C", "C", "C"],
        name: names,
        sc,
        title: [disp, when],
        log: game.kyokus.iter().map(Kyoku::dump).collect(),
    })
}

/// `total / 1000` as tenhou writes it: an integer when it is one.
fn thousandths(total: i64) -> Number {
    if total % 1000 == 0 {
        Number::from(total / 1000)
    } else {
        Number::from_f64(total as f64 / 1000.0).unwrap_or_else(|| Number::from(0))
    }
}

// ---- payload access (proto3 JSON: a missing field is its default) ----------

fn int(v: &JsonValue, key: &str) -> i64 {
    match v.get(key) {
        Some(JsonValue::Number(n)) => n.as_i64().unwrap_or(0),
        Some(JsonValue::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn flag(v: &JsonValue, key: &str) -> bool {
    v.get(key).and_then(JsonValue::as_bool).unwrap_or(false)
}

fn text<'a>(v: &'a JsonValue, key: &str) -> &'a str {
    v.get(key).and_then(JsonValue::as_str).unwrap_or("")
}

fn list<'a>(v: &'a JsonValue, key: &str) -> &'a [JsonValue] {
    v.get(key)
        .and_then(JsonValue::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn ints(v: &JsonValue, key: &str) -> Vec<i64> {
    list(v, key).iter().filter_map(JsonValue::as_i64).collect()
}

fn tiles(v: &JsonValue, key: &str) -> Result<Vec<Tile>> {
    list(v, key)
        .iter()
        .map(|t| Tile::parse(t.as_str().unwrap_or("")))
        .collect()
}

fn seat(v: &JsonValue) -> Result<usize> {
    let s = int(v, "seat");
    ensure!((0..4).contains(&s), "seat {s} out of range");
    Ok(s as usize)
}

// ---- tiles -----------------------------------------------------------------

const TSUMOGIRI: u32 = 60;
const HONOURS: u8 = 3;

/// A tile as Mahjong Soul writes it (`5m`, `0p` for a red five, `7z`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tile {
    num: u8,
    /// 0 man, 1 pin, 2 sou, 3 honours
    suit: u8,
}

impl Tile {
    fn parse(text: &str) -> Result<Tile> {
        let b = text.as_bytes();
        ensure!(b.len() == 2 && b[0].is_ascii_digit(), "bad tile {text:?}");
        let suit = match b[1].to_ascii_lowercase() {
            b'm' => 0,
            b'p' => 1,
            b's' => 2,
            b'z' => HONOURS,
            _ => bail!("bad tile {text:?}"),
        };
        Ok(Tile {
            num: b[0] - b'0',
            suit,
        })
    }

    /// tenhou's number: 11-19 man, 21-29 pin, 31-39 sou, 41-47 honours,
    /// 51/52/53 the red fives.
    fn code(self) -> u32 {
        let suit = u32::from(self.suit) + 1;
        if self.num != 0 {
            10 * suit + u32::from(self.num)
        } else {
            50 + suit
        }
    }

    /// The same tile with a red five read as a plain five.
    fn plain(self) -> Tile {
        if self.num == 0 && self.suit != HONOURS {
            Tile { num: 5, ..self }
        } else {
            self
        }
    }
}

// ---- draws and discards ----------------------------------------------------

/// One entry of a seat's draw or discard list.
#[derive(Debug, Clone)]
enum Entry {
    Draw(Tile),
    Discard {
        tile: Tile,
        tsumogiri: bool,
        riichi: bool,
    },
    Chi {
        a: Tile,
        b: Tile,
        called: Tile,
    },
    /// `from`: 0 the seat on the left, 1 opposite, 2 on the right.
    Pon {
        a: Tile,
        b: Tile,
        called: Tile,
        from: usize,
    },
    Daiminkan {
        a: Tile,
        b: Tile,
        c: Tile,
        called: Tile,
        from: usize,
    },
    /// The discard slot after a daiminkan, which tenhou fills with 0.
    AfterKan,
    Ankan(Tile),
    Kakan {
        a: Tile,
        b: Tile,
        c: Tile,
        added: Tile,
        from: usize,
    },
}

/// `tiles` as tenhou's numbers, with `mark` (a call letter and its tile)
/// inserted at `at`.
fn meld(tiles: &[Tile], at: usize, mark: String) -> JsonValue {
    let mut parts: Vec<String> = tiles.iter().map(|t| t.code().to_string()).collect();
    parts.insert(at, mark);
    JsonValue::String(parts.concat())
}

impl Entry {
    fn encode(&self) -> JsonValue {
        match *self {
            Entry::Draw(t) => json!(t.code()),
            Entry::Discard {
                tile,
                tsumogiri,
                riichi,
            } => {
                let v = if tsumogiri { TSUMOGIRI } else { tile.code() };
                if riichi {
                    json!(format!("r{v}"))
                } else {
                    json!(v)
                }
            }
            Entry::Chi { a, b, called } => {
                json!(format!("c{}{}{}", called.code(), a.code(), b.code()))
            }
            Entry::Pon { a, b, called, from } => meld(&[a, b], from, format!("p{}", called.code())),
            Entry::Daiminkan {
                a,
                b,
                c,
                called,
                from,
            } => {
                let at = if from == 2 { 3 } else { from };
                meld(&[a, b, c], at, format!("m{}", called.code()))
            }
            Entry::AfterKan => json!(0),
            Entry::Ankan(t) => {
                let c = t.code();
                if t.num == 5 && t.suit != HONOURS {
                    let red = Tile { num: 0, ..t }.code();
                    json!(format!("{red}{c}{c}a{c}"))
                } else {
                    json!(format!("{c}{c}{c}a{c}"))
                }
            }
            Entry::Kakan {
                a,
                b,
                c,
                added,
                from,
            } => meld(&[a, b, c], from, format!("k{}", added.code())),
        }
    }
}

// ---- results ---------------------------------------------------------------

const DAISANGEN: i64 = 37;
const DAISUUSHI: i64 = 50;
const TSUMO_OYA_PAYS_KO: i64 = 16000;
const TSUMO_KO_PAYS_OYA: i64 = 16000;
const TSUMO_KO_PAYS_KO: i64 = 8000;
const RON_OYA: i64 = 48000;
const RON_KO: i64 = 32000;
const WINDS: [&str; 4] = ["東", "南", "西", "北"];

#[derive(Debug, Clone)]
enum Outcome {
    /// An abortive draw, by tenhou's name.
    Abort(&'static str),
    Ryuukyoku {
        delta: [i64; 4],
        nagashi: bool,
    },
    Agari {
        wins: Vec<Win>,
        ura: Vec<Tile>,
        kyoku: i64,
    },
}

#[derive(Debug, Clone, Copy)]
struct Points {
    /// What each non-dealer pays on a tsumo.
    tsumo: i64,
    /// What the dealer pays on a non-dealer's tsumo.
    tsumo_oya: i64,
    ron: i64,
    oya: bool,
}

#[derive(Debug, Clone)]
struct Win {
    seat: usize,
    from: usize,
    pao: usize,
    han: i64,
    fu: i64,
    /// `(id, han)` per yaku.
    yaku: Vec<(i64, i64)>,
    tsumo: bool,
    yakuman: bool,
    points: Points,
    delta: [i64; 4],
}

/// Mahjong Soul's yaku ids, by tenhou's names. 10 and 11 (seat and round
/// wind) are named with the wind in `yaku_name`.
fn yaku_table(id: i64) -> Option<&'static str> {
    Some(match id {
        1 => "門前清自摸和",
        2 => "立直",
        3 => "槍槓",
        4 => "嶺上開花",
        5 => "海底摸月",
        6 => "河底撈魚",
        7 => "役牌 白",
        8 => "役牌 發",
        9 => "役牌 中",
        12 => "断幺九",
        13 => "一盃口",
        14 => "平和",
        15 => "混全帯幺九",
        16 => "一気通貫",
        17 => "三色同順",
        18 => "両立直",
        19 => "三色同刻",
        20 => "三槓子",
        21 => "対々和",
        22 => "三暗刻",
        23 => "小三元",
        24 => "混老頭",
        25 => "七対子",
        26 => "純全帯幺九",
        27 => "混一色",
        28 => "二盃口",
        29 => "清一色",
        30 => "一発",
        31 => "ドラ",
        32 => "赤ドラ",
        33 => "裏ドラ",
        34 => "抜きドラ",
        35 => "天和",
        36 => "地和",
        37 => "大三元",
        38 => "四暗刻",
        39 => "字一色",
        40 => "緑一色",
        41 => "清老頭",
        42 => "国士無双",
        43 => "小四喜",
        44 => "四槓子",
        45 => "九蓮宝燈",
        46 => "八連荘",
        47 => "純正九蓮宝燈",
        48 => "四暗刻単騎",
        49 => "国士無双十三面待ち",
        50 => "大四喜",
        51 => "燕返し",
        52 => "槓振り",
        53 => "十二落抬",
        54 => "五門斉",
        55 => "三連刻",
        56 => "一色三順",
        57 => "一筒摸月",
        58 => "九筒撈魚",
        59 => "人和",
        60 => "大車輪",
        61 => "大竹林",
        62 => "大数隣",
        63 => "石の上にも三年",
        64 => "大七星",
        _ => return None,
    })
}

fn yaku_name(id: i64, kyoku: i64, seat: usize) -> String {
    match id {
        10 => format!(
            "自風 {}",
            WINDS[(seat as i64 - kyoku).rem_euclid(4) as usize]
        ),
        11 => format!("場風 {}", WINDS[(kyoku / 4).rem_euclid(4) as usize]),
        _ => yaku_table(id).map_or_else(|| format!("yaku{id}"), str::to_string),
    }
}

impl Win {
    /// tenhou's score text: `30符2飜1000点`, `1000-2000点`, `2600点∀` for a
    /// dealer's tsumo, a limit name instead of the fu and han from mangan up.
    fn score_text(&self) -> String {
        let p = self.points;
        let pts = if self.tsumo {
            if p.oya {
                format!("{}点∀", p.tsumo)
            } else {
                format!("{}-{}点", p.tsumo, p.tsumo_oya)
            }
        } else {
            format!("{}点", p.ron)
        };
        // the hand's value as a non-dealer's
        let value = if p.ron == 0 {
            if p.oya {
                (p.tsumo * 2) as f64
            } else {
                (p.tsumo * 2 + p.tsumo_oya) as f64
            }
        } else if p.oya {
            p.ron as f64 / 1.5
        } else {
            p.ron as f64
        };
        let (han, fu) = (self.han, self.fu);
        let limit = if value >= 32000.0 {
            if han >= 13 {
                "数え役満"
            } else {
                "役満"
            }
        } else if value >= 24000.0 {
            "三倍満"
        } else if value >= 16000.0 {
            "倍満"
        } else if value >= 12000.0 {
            "跳満"
        } else if value >= 8000.0 {
            let full = han >= 5 || (han >= 4 && fu >= 40) || (han >= 3 && fu >= 70);
            if full {
                "満貫"
            } else {
                "切り上げ満貫"
            }
        } else {
            return format!("{fu}符{han}飜{pts}");
        };
        format!("{limit}{pts}")
    }
}

impl Outcome {
    fn dump(&self) -> JsonValue {
        match self {
            Outcome::Abort(name) => json!([name]),
            Outcome::Ryuukyoku { delta, nagashi } => {
                json!([if *nagashi { "流し満貫" } else { "流局" }, delta])
            }
            Outcome::Agari { wins, kyoku, .. } => {
                let mut out = vec![json!("和了")];
                for w in wins {
                    out.push(json!(w.delta));
                    let mut detail = vec![
                        json!(w.seat),
                        json!(w.from),
                        json!(w.pao),
                        json!(w.score_text()),
                    ];
                    for &(id, val) in &w.yaku {
                        let han = if w.yakuman {
                            "役満".to_string()
                        } else {
                            format!("{val}飜")
                        };
                        detail.push(json!(format!("{}({han})", yaku_name(id, *kyoku, w.seat))));
                    }
                    out.push(JsonValue::Array(detail));
                }
                JsonValue::Array(out)
            }
        }
    }
}

// ---- rounds ----------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Kyoku {
    /// 0 is East 1, 4 South 1.
    kyoku: i64,
    honba: i64,
    sticks: i64,
    start: Vec<i64>,
    doras: Vec<Tile>,
    haipai: [Vec<Tile>; 4],
    draws: [Vec<Entry>; 4],
    discards: [Vec<Entry>; 4],
    result: Option<Outcome>,
}

impl Kyoku {
    fn dump(&self) -> JsonValue {
        let codes = |ts: &[Tile]| JsonValue::Array(ts.iter().map(|t| json!(t.code())).collect());
        let ura = match &self.result {
            Some(Outcome::Agari { ura, .. }) => codes(ura),
            _ => json!([]),
        };
        let mut entry = vec![
            json!([self.kyoku, self.honba, self.sticks]),
            json!(self.start),
            codes(&self.doras),
            ura,
        ];
        let seats = self.haipai.iter().zip(&self.draws).zip(&self.discards);
        for ((haipai, draws), discards) in seats {
            entry.push(codes(haipai));
            entry.push(JsonValue::Array(draws.iter().map(Entry::encode).collect()));
            entry.push(JsonValue::Array(
                discards.iter().map(Entry::encode).collect(),
            ));
        }
        if let Some(result) = &self.result {
            entry.push(result.dump());
        }
        JsonValue::Array(entry)
    }
}

/// Walks a replay's records in order, building one `Kyoku` per round.
#[derive(Default)]
struct Game {
    kyokus: Vec<Kyoku>,
    cur: Option<Kyoku>,
    dealer: usize,
    /// The dealer's 14th tile, which tenhou counts as the first draw.
    popped: Option<Tile>,
    last_discarder: Option<usize>,
    /// Riichi sticks put down this round; `None` once a ron has taken them.
    riichi_count: Option<i64>,
    nkan: u32,
    riichi_pending: bool,
    winds_called: [u32; 4],
    dragons_called: [u32; 4],
    pao_wind: Option<usize>,
    pao_dragon: Option<usize>,
}

/// Where `feeder` sits as seen from `seat`: 0 left, 1 opposite, 2 right.
fn relative(seat: usize, feeder: usize) -> usize {
    (seat + 3 - feeder) % 4
}

/// The records a round is built from; `Game::feed` takes each of these.
const RECORDS: [&str; 8] = [
    "RecordNewRound",
    "RecordDealTile",
    "RecordDiscardTile",
    "RecordChiPengGang",
    "RecordAnGangAddGang",
    "RecordLiuJu",
    "RecordNoTile",
    "RecordHule",
];

impl Game {
    fn feed(&mut self, name: &str, m: &JsonValue) -> Result<()> {
        match name {
            "RecordNewRound" => self.new_round(m),
            "RecordDealTile" => self.deal(m),
            "RecordDiscardTile" => self.discard(m),
            "RecordChiPengGang" => self.call(m),
            "RecordAnGangAddGang" => self.kan(m),
            "RecordLiuJu" => self.abort(m),
            "RecordNoTile" => self.exhaustive(m),
            "RecordHule" => self.hule(m),
            _ => Ok(()),
        }
    }

    fn cur(&mut self) -> Result<&mut Kyoku> {
        self.cur.as_mut().context("record before the first round")
    }

    fn accept_riichi(&mut self) {
        if self.riichi_pending {
            self.riichi_pending = false;
            if let Some(n) = self.riichi_count.as_mut() {
                *n += 1;
            }
        }
    }

    fn see_doras(&mut self, m: &JsonValue, key: &str) -> Result<()> {
        let doras = tiles(m, key)?;
        let cur = self.cur()?;
        if doras.len() > cur.doras.len() {
            cur.doras = doras;
        }
        Ok(())
    }

    /// Count a called or kanned wind or dragon toward pao (daisuushi,
    /// daisangen): the seat that fed the completing set pays.
    fn count_pao(&mut self, t: Tile, owner: usize, feeder: Option<usize>) {
        if t.suit != HONOURS {
            return;
        }
        if (1..=4).contains(&t.num) {
            self.winds_called[owner] += 1;
            if self.winds_called[owner] == 4 {
                self.pao_wind = feeder;
            }
        } else if (5..=7).contains(&t.num) {
            self.dragons_called[owner] += 1;
            if self.dragons_called[owner] == 3 {
                self.pao_dragon = feeder;
            }
        }
    }

    fn new_round(&mut self, m: &JsonValue) -> Result<()> {
        let scores = ints(m, "scores");
        ensure!(
            scores.len() == 4,
            "only 4-player games convert (this one has {})",
            scores.len()
        );
        let dora = text(m, "dora");
        let doras = if dora.is_empty() {
            tiles(m, "doras")?
        } else {
            vec![Tile::parse(dora)?]
        };
        let mut haipai: [Vec<Tile>; 4] = Default::default();
        for (s, hand) in haipai.iter_mut().enumerate() {
            *hand = tiles(m, &format!("tiles{s}"))?;
        }
        let dealer = int(m, "ju");
        ensure!((0..4).contains(&dealer), "dealer {dealer} out of range");
        let dealer = dealer as usize;
        let popped = haipai[dealer].pop();
        let mut draws: [Vec<Entry>; 4] = Default::default();
        if let Some(t) = popped {
            draws[dealer].push(Entry::Draw(t));
        }
        self.cur = Some(Kyoku {
            kyoku: 4 * int(m, "chang") + int(m, "ju"),
            honba: int(m, "ben"),
            sticks: int(m, "liqibang"),
            start: scores,
            doras,
            haipai,
            draws,
            ..Kyoku::default()
        });
        self.dealer = dealer;
        self.popped = popped;
        self.last_discarder = None;
        self.riichi_count = Some(0);
        self.nkan = 0;
        self.riichi_pending = false;
        self.winds_called = [0; 4];
        self.dragons_called = [0; 4];
        self.pao_wind = None;
        self.pao_dragon = None;
        Ok(())
    }

    fn deal(&mut self, m: &JsonValue) -> Result<()> {
        self.accept_riichi();
        self.see_doras(m, "doras")?;
        let s = seat(m)?;
        let t = Tile::parse(text(m, "tile"))?;
        self.cur()?.draws[s].push(Entry::Draw(t));
        Ok(())
    }

    fn discard(&mut self, m: &JsonValue) -> Result<()> {
        let s = seat(m)?;
        let tile = Tile::parse(text(m, "tile"))?;
        let mut tsumogiri = flag(m, "moqie");
        let first_by_dealer = s == self.dealer && self.cur()?.discards[s].is_empty();
        // Mahjong Soul deals the dealer 14 tiles; tenhou draws the 14th, so
        // throwing that tile first is a tsumogiri there.
        if first_by_dealer && self.popped == Some(tile) {
            tsumogiri = true;
        }
        let riichi = flag(m, "is_liqi");
        self.cur()?.discards[s].push(Entry::Discard {
            tile,
            tsumogiri,
            riichi,
        });
        if riichi {
            self.riichi_pending = true;
        }
        self.last_discarder = Some(s);
        self.see_doras(m, "doras")
    }

    fn call(&mut self, m: &JsonValue) -> Result<()> {
        self.accept_riichi();
        let s = seat(m)?;
        let ts = tiles(m, "tiles")?;
        let kind = int(m, "type");
        let feeder = self
            .last_discarder
            .context("call with no discard before it")?;
        let from = relative(s, feeder);
        match kind {
            0 => {
                ensure!(ts.len() >= 3, "chi with {} tiles", ts.len());
                self.cur()?.draws[s].push(Entry::Chi {
                    a: ts[0],
                    b: ts[1],
                    called: ts[2],
                });
            }
            1 => {
                ensure!(ts.len() >= 3, "pon with {} tiles", ts.len());
                self.count_pao(ts[0], s, Some(feeder));
                self.cur()?.draws[s].push(Entry::Pon {
                    a: ts[0],
                    b: ts[1],
                    called: ts[2],
                    from,
                });
            }
            2 => {
                ensure!(ts.len() >= 4, "kan with {} tiles", ts.len());
                self.count_pao(ts[0], s, Some(feeder));
                let cur = self.cur()?;
                cur.draws[s].push(Entry::Daiminkan {
                    a: ts[0],
                    b: ts[1],
                    c: ts[2],
                    called: ts[3],
                    from,
                });
                cur.discards[s].push(Entry::AfterKan);
                self.nkan += 1;
            }
            _ => bail!("unknown RecordChiPengGang.type {kind}"),
        }
        Ok(())
    }

    fn kan(&mut self, m: &JsonValue) -> Result<()> {
        let s = seat(m)?;
        let kind = int(m, "type");
        let t = Tile::parse(text(m, "tiles"))?;
        self.last_discarder = Some(s);
        match kind {
            3 => {
                self.count_pao(t, s, None);
                self.cur()?.discards[s].push(Entry::Ankan(t.plain()));
                self.nkan += 1;
            }
            2 => {
                let cur = self.cur()?;
                let pon = cur.draws[s].iter().find_map(|e| match *e {
                    Entry::Pon { a, b, called, from } if called.plain() == t.plain() => {
                        Some((a, b, called, from))
                    }
                    _ => None,
                });
                if let Some((a, b, called, from)) = pon {
                    cur.discards[s].push(Entry::Kakan {
                        a,
                        b,
                        c: called,
                        added: t,
                        from,
                    });
                    self.nkan += 1;
                }
            }
            _ => bail!("unknown RecordAnGangAddGang.type {kind}"),
        }
        Ok(())
    }

    fn finish(&mut self, result: Outcome) -> Result<()> {
        let mut kyoku = self.cur.take().context("round end before the round")?;
        kyoku.result = Some(result);
        self.kyokus.push(kyoku);
        Ok(())
    }

    fn abort(&mut self, m: &JsonValue) -> Result<()> {
        self.accept_riichi();
        let name = match int(m, "type") {
            1 => "九種九牌",
            2 => "四風連打",
            3 => "四槓散了",
            4 => "四家立直",
            5 => "三家和了",
            other => bail!("unknown RecordLiuJu.type {other}"),
        };
        self.finish(Outcome::Abort(name))
    }

    fn exhaustive(&mut self, m: &JsonValue) -> Result<()> {
        let mut delta = [0i64; 4];
        for score in list(m, "scores") {
            for (d, x) in delta.iter_mut().zip(ints(score, "delta_scores")) {
                *d += x;
            }
        }
        self.finish(Outcome::Ryuukyoku {
            delta,
            nagashi: flag(m, "liujumanguan"),
        })
    }

    fn hule(&mut self, m: &JsonValue) -> Result<()> {
        let mut wins = Vec::new();
        let mut ura = Vec::new();
        for h in list(m, "hules") {
            let li = tiles(h, "li_doras")?;
            if li.len() > ura.len() {
                ura = li;
            }
            wins.push(self.win(h)?);
        }
        let kyoku = self.cur()?.kyoku;
        self.finish(Outcome::Agari { wins, ura, kyoku })
    }

    fn win(&mut self, h: &JsonValue) -> Result<Win> {
        let (sticks, honba) = {
            let cur = self.cur()?;
            (cur.sticks, cur.honba)
        };
        // the first ron winner takes the riichi sticks and the honba
        let (rp, hb) = match self.riichi_count {
            Some(n) => (1000 * (n + sticks), 100 * honba),
            None => (0, 0),
        };
        let s = seat(h)?;
        let tsumo = flag(h, "zimo");
        let oya = flag(h, "qinjia");
        let yakuman = flag(h, "yiman");
        let rong = int(h, "point_rong");
        let zqin = int(h, "point_zimo_qin");
        let zxian = int(h, "point_zimo_xian");
        let yaku: Vec<(i64, i64)> = list(h, "fans")
            .iter()
            .map(|f| (int(f, "id"), int(f, "val")))
            .collect();

        let mut pao = None;
        if yakuman {
            for &(id, val) in &yaku {
                if id == DAISUUSHI && self.pao_wind.is_some() {
                    pao = self.pao_wind.map(|p| (p, val));
                    break;
                }
                if id == DAISANGEN && self.pao_dragon.is_some() {
                    pao = self.pao_dragon.map(|p| (p, val));
                    break;
                }
            }
        }

        let dealer = self.dealer;
        let mut delta;
        let points;
        let from;
        if tsumo {
            from = s;
            delta = [-hb - zxian; 4];
            if s == dealer {
                delta[s] = rp + 3 * (hb + zxian);
                points = Points {
                    tsumo: zxian,
                    tsumo_oya: 0,
                    ron: 0,
                    oya: true,
                };
            } else {
                delta[s] = rp + hb + zqin + 2 * (hb + zxian);
                delta[dealer] = -hb - zqin;
                points = Points {
                    tsumo: zxian,
                    tsumo_oya: zqin,
                    ron: 0,
                    oya: false,
                };
            }
        } else {
            from = self
                .last_discarder
                .context("ron with no discard before it")?;
            delta = [0; 4];
            delta[s] = rp + 3 * hb + rong;
            delta[from] = -3 * hb - rong;
            points = Points {
                tsumo: 0,
                tsumo_oya: 0,
                ron: rong,
                oya,
            };
            self.riichi_count = None;
        }
        if let Some((liable, times)) = pao {
            if !tsumo {
                let pay = 3 * hb + times * (if oya { RON_OYA } else { RON_KO }) / 2;
                delta[liable] -= pay;
                delta[from] += pay;
            } else if oya {
                delta[liable] -= 2 * hb + times * 2 * TSUMO_OYA_PAYS_KO;
                for (i, d) in delta.iter_mut().enumerate() {
                    if i != liable && i != s {
                        *d += hb + times * TSUMO_OYA_PAYS_KO;
                    }
                }
            } else {
                delta[liable] -= 2 * hb + times * (TSUMO_KO_PAYS_OYA + TSUMO_KO_PAYS_KO);
                for (i, d) in delta.iter_mut().enumerate() {
                    if i != liable && i != s {
                        *d += hb
                            + times
                                * (if i == dealer {
                                    TSUMO_KO_PAYS_OYA
                                } else {
                                    TSUMO_KO_PAYS_KO
                                });
                    }
                }
            }
        }
        Ok(Win {
            seat: s,
            from,
            pao: pao.map_or(s, |(p, _)| p),
            han: int(h, "count"),
            fu: int(h, "fu"),
            yaku,
            tsumo,
            yakuman,
            points,
            delta,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Tile {
        Tile::parse(s).unwrap()
    }

    /// The fixture of honvl/Majsoul-to-NAGA (MIT): a serialized
    /// `ResGameRecord` and the log its converter, checked against tensoul-py,
    /// makes of it.
    #[test]
    fn converts_the_reference_record() {
        let bytes = BASE64
            .decode(include_str!("testdata/naga_sample.res.b64").trim())
            .unwrap();
        let response = decode_message("lq.ResGameRecord", &bytes).unwrap();
        let log = from_record(&response).unwrap();
        let expected: JsonValue =
            serde_json::from_str(include_str!("testdata/naga_sample.expected.json")).unwrap();
        assert_eq!(JsonValue::Array(log.log.clone()), expected);
        assert_eq!(log.reference, "sample");
        assert_eq!(log.naga_text().lines().count(), log.log.len());
        for line in log.naga_text().lines() {
            let round: JsonValue =
                serde_json::from_str(line.strip_prefix(NAGA_PREFIX).unwrap()).unwrap();
            assert_eq!(round["log"].as_array().unwrap().len(), 1);
            assert_eq!(round["rule"]["aka"], 1);
        }
    }

    #[test]
    fn tiles_use_tenhou_numbers() {
        assert_eq!(t("1m").code(), 11);
        assert_eq!(t("9s").code(), 39);
        assert_eq!(t("7z").code(), 47);
        assert_eq!(t("0m").code(), 51);
        assert_eq!(t("0p").code(), 52);
        assert_eq!(t("0s").code(), 53);
        assert_eq!(t("0s").plain(), t("5s"));
        assert!(Tile::parse("5x").is_err());
    }

    #[test]
    fn calls_mark_where_the_tile_came_from() {
        let (a, b, c) = (t("4z"), t("4z"), t("4z"));
        let pon = |from| {
            Entry::Pon {
                a,
                b,
                called: c,
                from,
            }
            .encode()
        };
        assert_eq!(pon(0), json!("p444444"));
        assert_eq!(pon(1), json!("44p4444"));
        assert_eq!(pon(2), json!("4444p44"));
        let kan = |from| {
            Entry::Daiminkan {
                a,
                b,
                c,
                called: c,
                from,
            }
            .encode()
        };
        assert_eq!(kan(0), json!("m44444444"));
        assert_eq!(kan(1), json!("44m444444"));
        assert_eq!(kan(2), json!("444444m44"));
        let kakan = Entry::Kakan {
            a,
            b,
            c,
            added: c,
            from: 1,
        }
        .encode();
        assert_eq!(kakan, json!("44k444444"));
        let chi = Entry::Chi {
            a: t("3m"),
            b: t("4m"),
            called: t("2m"),
        }
        .encode();
        assert_eq!(chi, json!("c121314"));
        assert_eq!(Entry::Ankan(t("0p").plain()).encode(), json!("522525a25"));
        assert_eq!(Entry::Ankan(t("1z")).encode(), json!("414141a41"));
        let riichi = Entry::Discard {
            tile: t("3s"),
            tsumogiri: true,
            riichi: true,
        };
        assert_eq!(riichi.encode(), json!("r60"));
    }

    fn win(han: i64, fu: i64, points: Points) -> Win {
        Win {
            seat: 0,
            from: 0,
            pao: 0,
            han,
            fu,
            yaku: Vec::new(),
            tsumo: points.ron == 0,
            yakuman: false,
            points,
            delta: [0; 4],
        }
    }

    #[test]
    fn score_text_names_limits() {
        let ron = |ron, oya| Points {
            tsumo: 0,
            tsumo_oya: 0,
            ron,
            oya,
        };
        assert_eq!(win(2, 30, ron(2000, false)).score_text(), "30符2飜2000点");
        assert_eq!(
            win(4, 30, ron(8000, false)).score_text(),
            "切り上げ満貫8000点"
        );
        assert_eq!(win(4, 40, ron(8000, false)).score_text(), "満貫8000点");
        assert_eq!(win(6, 30, ron(18000, true)).score_text(), "跳満18000点");
        let tsumo = Points {
            tsumo: 2600,
            tsumo_oya: 0,
            ron: 0,
            oya: true,
        };
        assert_eq!(win(2, 40, tsumo).score_text(), "40符2飜2600点∀");
        let tsumo = Points {
            tsumo: 1000,
            tsumo_oya: 2000,
            ron: 0,
            oya: false,
        };
        assert_eq!(win(3, 30, tsumo).score_text(), "30符3飜1000-2000点");
        assert_eq!(yaku_name(11, 4, 2), "場風 南");
        assert_eq!(yaku_name(10, 5, 2), "自風 南");
        assert_eq!(yaku_name(999, 0, 0), "yaku999");
    }

    fn record(name: &str, body: JsonValue) -> (String, JsonValue) {
        (name.to_string(), body)
    }

    fn play(records: &[(String, JsonValue)]) -> Result<Vec<JsonValue>> {
        let mut game = Game::default();
        for (name, body) in records {
            game.feed(name, body)?;
        }
        Ok(game.kyokus.iter().map(Kyoku::dump).collect())
    }

    fn new_round(ju: u32) -> (String, JsonValue) {
        let hand = |s: &str| json!(s.split(' ').collect::<Vec<_>>());
        record(
            "RecordNewRound",
            json!({
                "chang": 0, "ju": ju, "ben": 0, "liqibang": 0, "dora": "", "doras": ["1m"],
                "scores": [25000, 25000, 25000, 25000],
                "tiles0": hand("5z 5z 6z 6z 1m 2m 3m 4m 5m 6m 7m 8m 9m 9m"),
                "tiles1": hand("7z 7z 1p 2p 3p 4p 5p 6p 7p 8p 9p 1s 2s"),
                "tiles2": hand("5z 6z 7z 1s 2s 3s 4s 5s 6s 7s 8s 9s 9s"),
                "tiles3": hand("1z 2z 3z 4z 1p 1p 9p 9p 1s 1s 9s 9s 2m"),
            }),
        )
    }

    /// Daisangen fed by seat 2's dragons: the liable seat shares the ron.
    #[test]
    fn pao_splits_a_yakuman_ron() {
        let records = vec![
            new_round(0),
            record(
                "RecordDiscardTile",
                json!({"seat": 0, "tile": "9m", "moqie": false, "is_liqi": false}),
            ),
            record("RecordDealTile", json!({"seat": 1, "tile": "3s"})),
            record(
                "RecordDiscardTile",
                json!({"seat": 1, "tile": "7z", "moqie": false, "is_liqi": false}),
            ),
            record(
                "RecordChiPengGang",
                json!({"seat": 0, "type": 1, "tiles": ["7z", "7z", "7z"]}),
            ),
            record(
                "RecordDiscardTile",
                json!({"seat": 0, "tile": "2m", "moqie": false, "is_liqi": false}),
            ),
            record("RecordDealTile", json!({"seat": 1, "tile": "4s"})),
            record(
                "RecordDiscardTile",
                json!({"seat": 1, "tile": "4s", "moqie": true, "is_liqi": false}),
            ),
            record("RecordDealTile", json!({"seat": 2, "tile": "5z"})),
            record(
                "RecordDiscardTile",
                json!({"seat": 2, "tile": "5z", "moqie": true, "is_liqi": false}),
            ),
            record(
                "RecordChiPengGang",
                json!({"seat": 0, "type": 1, "tiles": ["5z", "5z", "5z"]}),
            ),
            record(
                "RecordDiscardTile",
                json!({"seat": 0, "tile": "3m", "moqie": false, "is_liqi": false}),
            ),
            record("RecordDealTile", json!({"seat": 1, "tile": "5s"})),
            record(
                "RecordDiscardTile",
                json!({"seat": 1, "tile": "5s", "moqie": true, "is_liqi": false}),
            ),
            record("RecordDealTile", json!({"seat": 2, "tile": "6z"})),
            record(
                "RecordDiscardTile",
                json!({"seat": 2, "tile": "6z", "moqie": true, "is_liqi": false}),
            ),
            record(
                "RecordChiPengGang",
                json!({"seat": 0, "type": 1, "tiles": ["6z", "6z", "6z"]}),
            ),
            record(
                "RecordDiscardTile",
                json!({"seat": 0, "tile": "4m", "moqie": false, "is_liqi": false}),
            ),
            record("RecordDealTile", json!({"seat": 1, "tile": "6s"})),
            record(
                "RecordDiscardTile",
                json!({"seat": 1, "tile": "9m", "moqie": false, "is_liqi": false}),
            ),
            record(
                "RecordHule",
                json!({"hules": [{
                    "seat": 0, "zimo": false, "qinjia": true, "yiman": true, "count": 1, "fu": 40,
                    "point_rong": 48000, "li_doras": [], "fans": [{"id": 37, "val": 1}]
                }]}),
            ),
        ];
        let log = play(&records).unwrap();
        let result = &log[0][16];
        // seat 1 dealt in; seat 2 completed the dragons and pays half
        assert_eq!(result[0], json!("和了"));
        assert_eq!(result[1], json!([48000, -24000, -24000, 0]));
        assert_eq!(result[2], json!([0, 1, 2, "役満48000点", "大三元(役満)"]));
        // the dealer's 14th tile is tenhou's first draw, so throwing it is a tsumogiri
        assert_eq!(log[0][5][0], json!(19));
        assert_eq!(log[0][6][0], json!(60));
        // seat 0's pons: from seat 1 (its right), then seat 2 (opposite)
        assert_eq!(log[0][5][1], json!("4747p47"));
        assert_eq!(log[0][5][2], json!("45p4545"));
    }

    #[test]
    fn abortive_draws_use_tenhou_names() {
        let log = play(&[new_round(0), record("RecordLiuJu", json!({"type": 3}))]).unwrap();
        assert_eq!(log[0][16], json!(["四槓散了"]));
        let log = play(&[new_round(0), record("RecordLiuJu", json!({"type": 5}))]).unwrap();
        assert_eq!(log[0][16], json!(["三家和了"]));
        assert!(play(&[new_round(0), record("RecordLiuJu", json!({"type": 9}))]).is_err());
    }

    #[test]
    fn three_player_games_are_refused() {
        let (name, mut body) = new_round(0);
        body["scores"] = json!([35000, 35000, 35000]);
        let err = play(&[(name, body)]).unwrap_err();
        assert!(err.to_string().contains("4-player"));
    }

    #[test]
    fn export_writes_the_log_and_naga_lines() {
        let bytes = BASE64
            .decode(include_str!("testdata/naga_sample.res.b64").trim())
            .unwrap();
        let response = decode_message("lq.ResGameRecord", &bytes).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = export(dir.path(), &response).unwrap();
        assert_eq!(out.uuid, "sample");
        assert_eq!(out.log, dir.path().join("sample.json"));
        let naga = std::fs::read_to_string(&out.naga).unwrap();
        assert_eq!(naga.lines().count(), out.rounds);
        assert!(naga.lines().all(|l| l.starts_with(NAGA_PREFIX)));
        let log: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&out.log).unwrap()).unwrap();
        assert_eq!(log["ref"], "sample");
    }

    #[test]
    fn a_record_behind_a_url_is_reported() {
        let response = json!({"head": {"uuid": "x"}, "data": "", "data_url": "https://example.invalid/r?sig=fake"});
        let err = from_record(&response).unwrap_err().to_string();
        assert!(err.contains("not inline"));
        assert!(
            err.contains("example.invalid/r") && !err.contains("fake"),
            "{err}"
        );
    }
}
