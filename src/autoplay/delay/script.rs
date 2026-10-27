//! Lua override for the pre-click delay policy.
//!
//! A Bot policy or explicit profile override defines:
//!
//! ```lua
//! function decide_delay(ctx)
//!   return { delay_ms = 2300, allow_bank = false, hesitate = false }
//! end
//! ```
//!
//! `delay_ms` is the target **total** thinking time for the decision
//! window (the interval the server observes), not a sleep length — the
//! caller subtracts time already consumed.
//!
//! Safety contract (all enforced here, not trusted to the script):
//! - Restricted stdlib: math/string/table plus a scrubbed base library —
//!   no io/os/debug, no `load`/`dofile`/`loadfile` (bytecode and
//!   filesystem escapes), no `pcall`/`xpcall` (they could swallow the
//!   runaway-guard abort), no `string.dump`.
//! - A hard allocation ceiling ([`MEMORY_LIMIT_BYTES`]) — allocation
//!   failure becomes a normal Lua error, not a process abort.
//! - Instruction budget + wall-clock deadline via a VM hook, active
//!   both while the chunk's top level runs at load time and during
//!   every `decide_delay` call.
//! - Any failure (missing file is not a failure; syntax error, runtime
//!   error, timeout, wrong return shape, out-of-range delay) falls back
//!   to the built-in model and is logged **once** per distinct error,
//!   not once per hand.
//! - The result is clamped by [`budget_cap`] and [`functional_floor`] —
//!   a script can neither overrun the server clock nor click into a
//!   dealing animation.
//! - The script never sees or influences the chosen action.

use super::PolicySource;
use super::{budget_cap, functional_floor, DelayDecision, DelayInput};
use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value, VmState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tracing::{info, warn};

/// Embedded default when a Bot declares no policy. Never generated in each
/// account: declared Bot policies and explicit overrides own their provenance.
pub const DEFAULT_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/delay_default.lua"
));

/// Every bundled default shipped by an earlier version, verbatim. An
/// existing `delay.lua` identical (modulo line endings) to one of these
/// is an untouched auto-generated copy of an outdated default, so
/// migration can identify it as generated rather than an explicit override.
/// Anything else is a user edit and must be preserved by the migration owner.
///
/// When changing `assets/delay_default.lua`, copy the version being
/// replaced into `assets/delay_default_superseded/<short-commit>.lua`
/// and add it here — otherwise existing installs keep the old script.
const SUPERSEDED_DEFAULT_SCRIPTS: &[&str] = &[
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/delay_default_superseded/f8c7cfd.lua"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/delay_default_superseded/1893c05.lua"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/delay_default_superseded/cb1f146.lua"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/delay_default_superseded/f1f2842.lua"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/delay_default_superseded/2e5d892.lua"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/delay_default_superseded/68bb28c.lua"
    )),
];

/// Is `content` an unmodified copy of an earlier bundled default? Line
/// endings are normalized on both sides: the file on disk or the
/// compiled-in constant may carry CRLF depending on the checkout and
/// platform, and a pure EOL difference is not a user edit.
pub fn is_superseded_default(content: &str) -> bool {
    let normalized = content.replace("\r\n", "\n");
    SUPERSEDED_DEFAULT_SCRIPTS
        .iter()
        .any(|old| old.replace("\r\n", "\n") == normalized)
}

/// Hard sanity range for a script-provided target (10 minutes). Values
/// outside are treated as a script bug, not clamped silently.
const MAX_SCRIPT_DELAY_MS: f64 = 600_000.0;
/// Wall-clock deadline for one script call.
const CALL_DEADLINE: Duration = Duration::from_millis(50);
/// Wall-clock deadline for running the chunk's top level at load time.
/// More generous than [`CALL_DEADLINE`]: precomputing tables at file
/// scope is legitimate, and a load happens once per edit, not per hand.
const COMPILE_DEADLINE: Duration = Duration::from_millis(250);
/// The VM hook fires every N instructions to check the deadline.
const HOOK_EVERY_N_INSTRUCTIONS: u32 = 1_000;
/// Instruction budget for one call (hook fires × N).
const MAX_HOOK_FIRES: u64 = 10_000; // = 10M instructions
/// Lua allocation ceiling. Far above anything a delay policy needs, but
/// it turns a runaway `string.rep`-style allocation into a catchable
/// Lua error — without a limit, mlua's allocator aborts the whole
/// process when the system allocation fails, and the VM hook cannot
/// intervene because C functions never tick it.
const MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
/// Base-library globals scrubbed from the VM. `Lua::new_with` always
/// loads the base library regardless of the `StdLib` mask, and these
/// break the sandbox or the runaway guard: `load`/`loadstring` accept
/// precompiled bytecode which Lua 5.4 does not validate (crafted
/// bytecode is native-code execution), `dofile`/`loadfile` reach the
/// filesystem (`dofile()` with no argument blocks reading stdin), and
/// `pcall`/`xpcall` would let a script catch the guard's abort error
/// and keep spinning.
const SCRUBBED_GLOBALS: &[&str] = &[
    "load",
    "loadstring",
    "dofile",
    "loadfile",
    "pcall",
    "xpcall",
];

/// A loaded, compiled delay script.
pub struct DelayScript {
    lua: Lua,
    func: Function,
    /// Last error reported for this script instance; used to log each
    /// distinct failure once instead of spamming every hand. Mutex (not
    /// RefCell) because `&DelayScript` is held inside `ActionContext`
    /// across an await, which requires `Sync`; it is never contended.
    last_error: std::sync::Mutex<Option<String>>,
}

/// Install the runaway guard (instruction budget + wall-clock deadline)
/// on `lua`. The hook only ticks pure Lua execution — C functions never
/// fire it — so allocation is bounded separately by the memory limit.
fn install_guard(lua: &Lua, budget: Duration) -> Result<(), mlua::Error> {
    let deadline = Instant::now() + budget;
    let fires = AtomicU64::new(0);
    lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(HOOK_EVERY_N_INSTRUCTIONS),
        move |_lua, _debug| {
            if fires.fetch_add(1, Ordering::Relaxed) >= MAX_HOOK_FIRES {
                return Err(mlua::Error::RuntimeError(
                    "delay script exceeded instruction budget".into(),
                ));
            }
            if Instant::now() > deadline {
                return Err(mlua::Error::RuntimeError(
                    "delay script exceeded time budget".into(),
                ));
            }
            Ok(VmState::Continue)
        },
    )
}

impl DelayScript {
    /// Compile `source` and resolve the `decide_delay` global.
    pub fn compile(source: &str, chunk_name: &str) -> Result<Self, String> {
        let lua = Lua::new_with(
            StdLib::MATH | StdLib::STRING | StdLib::TABLE,
            LuaOptions::default(),
        )
        .map_err(|e| format!("lua init: {e}"))?;
        let globals = lua.globals();
        for name in SCRUBBED_GLOBALS {
            globals
                .raw_set(*name, Value::Nil)
                .map_err(|e| format!("lua sandbox: {e}"))?;
        }
        // Bytecode has no legitimate producer once `load` is gone, but
        // strip the producer too so none ever exists in this VM.
        if let Ok(string_lib) = globals.get::<Table>("string") {
            let _ = string_lib.raw_set("dump", Value::Nil);
        }
        lua.set_memory_limit(MEMORY_LIMIT_BYTES)
            .map_err(|e| format!("lua memory limit: {e}"))?;
        // The chunk's top level is user code too: run it under the same
        // guard as decide_delay calls, so `while true do end` at file
        // scope rejects the script instead of hanging the autoplay task.
        install_guard(&lua, COMPILE_DEADLINE).map_err(|e| format!("lua hook: {e}"))?;
        let loaded = lua.load(source).set_name(chunk_name).exec();
        lua.remove_hook();
        loaded.map_err(|e| format!("script load: {e}"))?;
        let func: Function = lua
            .globals()
            .get("decide_delay")
            .map_err(|_| "script defines no `decide_delay` function".to_string())?;
        Ok(Self {
            lua,
            func,
            last_error: std::sync::Mutex::new(None),
        })
    }

    /// Run the script for one decision. `None` means "fall back to the
    /// built-in model" (and the cause has been logged once).
    pub fn try_decide(&self, input: &DelayInput) -> Option<DelayDecision> {
        if install_guard(&self.lua, CALL_DEADLINE).is_err() {
            // No hook means no runaway protection — refuse to run the
            // script rather than risk blocking the autoplay loop.
            warn!("delay script: could not install VM hook — using built-in model");
            return None;
        }
        let result = self.call(input);
        self.lua.remove_hook();

        match result {
            Ok((target_ms, allow_bank, hesitate, look_ms)) => {
                if let Ok(mut g) = self.last_error.lock() {
                    *g = None;
                }
                // Enforce the non-negotiables on the script's answer.
                let target = target_ms
                    .min(budget_cap(input, allow_bank))
                    .max(functional_floor(input));
                Some(DelayDecision {
                    total_target_ms: target,
                    allow_bank,
                    hesitate,
                    look_ms,
                })
            }
            Err(e) => {
                let msg = e.to_string();
                if let Ok(mut g) = self.last_error.lock() {
                    if g.as_deref() != Some(msg.as_str()) {
                        warn!("delay script failed — using built-in model: {msg}");
                        *g = Some(msg);
                    }
                }
                None
            }
        }
    }

    fn call(&self, input: &DelayInput) -> mlua::Result<(u32, bool, bool, Option<u32>)> {
        let ctx = self.build_ctx(input)?;
        let ret: Value = self.func.call(ctx)?;
        let Value::Table(t) = ret else {
            return Err(mlua::Error::RuntimeError(
                "decide_delay must return a table { delay_ms, allow_bank }".into(),
            ));
        };
        let delay_ms: f64 = t.get("delay_ms").map_err(|_| {
            mlua::Error::RuntimeError("decide_delay result is missing numeric `delay_ms`".into())
        })?;
        if !delay_ms.is_finite() || !(0.0..=MAX_SCRIPT_DELAY_MS).contains(&delay_ms) {
            return Err(mlua::Error::RuntimeError(format!(
                "decide_delay returned out-of-range delay_ms {delay_ms}"
            )));
        }
        // Strict booleans: Lua truthiness would read `allow_bank = 0`
        // (C-style false) as *true* and silently unlock bank spending.
        let flag = |name: &str| match t.get::<Value>(name) {
            Ok(Value::Nil) | Err(_) => Ok(false),
            Ok(Value::Boolean(b)) => Ok(b),
            Ok(other) => Err(mlua::Error::RuntimeError(format!(
                "decide_delay `{name}` must be a boolean or nil, got {}",
                other.type_name()
            ))),
        };
        // How long to rest on a tile that is looked at: a tenth of a second
        // to five, or the script has a bug.
        let look_ms = match t.get::<Value>("look_ms") {
            Ok(Value::Nil) | Err(_) => None,
            Ok(Value::Integer(n)) if (100..=5_000).contains(&n) => Some(n as u32),
            Ok(Value::Number(n)) if (100.0..=5_000.0).contains(&n) => Some(n as u32),
            Ok(other) => {
                return Err(mlua::Error::RuntimeError(format!(
                    "decide_delay `look_ms` must be a number from 100 to 5000, got {other:?}"
                )));
            }
        };
        Ok((
            delay_ms as u32,
            flag("allow_bank")?,
            flag("hesitate")?,
            look_ms,
        ))
    }

    /// Build the read-only `ctx` table the script receives.
    fn build_ctx(&self, input: &DelayInput) -> mlua::Result<Table> {
        let lua = &self.lua;
        let ctx = lua.create_table()?;
        ctx.set("action", kind_name(input.kind))?;
        ctx.set("tsumogiri", input.is_tsumogiri)?;
        ctx.set("post_call", input.is_post_call)?;
        ctx.set("first_action", input.first_action_of_kyoku)?;
        ctx.set("dealer_opening", input.opening_animation)?;
        ctx.set("can_riichi", input.can_riichi)?;
        ctx.set("is_kan", input.kind.is_kan())?;
        ctx.set("in_riichi", input.in_riichi)?;
        ctx.set("opponent_riichi", input.opponent_riichi)?;
        if let Some(tc) = input.tile_class {
            ctx.set("tile_class", tc.as_str())?;
        }
        ctx.set("junme", input.junme)?;
        ctx.set("legal_count", input.legal_action_count)?;
        let reading = &input.reading;
        if let Some(loose) = reading.loose {
            ctx.set("loose", loose)?;
        }
        if let Some(loose) = reading.runner_up_loose {
            ctx.set("runner_up_loose", loose)?;
        }
        if let Some(safe) = reading.safe {
            ctx.set("safe", safe)?;
        }
        if let Some(human) = reading.human {
            ctx.set("human_top", human.top)?;
            ctx.set("human_chosen", human.chosen)?;
            if let Some(second) = human.second {
                ctx.set("human_second", second)?;
            }
        }
        if let Some(p) = input.probs {
            ctx.set("top_prob", p.top)?;
            ctx.set("entropy", p.entropy)?;
            if let Some(second) = p.second {
                ctx.set("second_prob", second)?;
            }
            if let Some(margin) = p.margin() {
                ctx.set("margin", margin)?;
            }
        }
        if let Some(b) = input.budget {
            let budget = lua.create_table()?;
            budget.set("fixed_ms", b.fixed_ms)?;
            budget.set("add_ms", b.add_ms)?;
            budget.set("elapsed_ms", b.elapsed_ms)?;
            ctx.set("budget", budget)?;
        }
        // The script's draws are autoplay's like any other (`autoplay::humanize`).
        ctx.set(
            "rng",
            lua.create_function(|_, ()| Ok(crate::autoplay::humanize::fraction(&mut rand::rng())))?,
        )?;
        ctx.set(
            "lognormal",
            lua.create_function(|_, (mu, sigma): (f64, f64)| {
                match crate::autoplay::humanize::log_normal(mu, sigma) {
                    Some(dist) => {
                        use rand::Rng;
                        Ok(rand::rng().sample::<f64, _>(dist))
                    }
                    None => Err(mlua::Error::RuntimeError("lognormal: invalid sigma".into())),
                }
            })?,
        )?;
        Ok(ctx)
    }
}

fn kind_name(kind: super::DecisionKind) -> &'static str {
    use super::DecisionKind::*;
    match kind {
        Dahai => "dahai",
        Reach => "reach",
        Chi => "chi",
        Pon => "pon",
        Daiminkan => "daiminkan",
        Ankan => "ankan",
        Kakan => "kakan",
        Hora => "hora",
        Ryukyoku => "ryukyoku",
        Kita => "kita",
        Pass => "none",
    }
}

/// Owns a resolved policy and its hot-reload state. No implicit profile file
/// is generated or searched; failed declared sources remain observable.
#[derive(Default)]
pub struct ScriptHost {
    script: Option<DelayScript>,
    attempted_source: Option<PolicySource>,
    attempted_stamp: Option<(SystemTime, u64)>,
    load_error: Option<String>,
}

impl ScriptHost {
    /// Drop policy state after a reported manifest/source resolution failure.
    /// PacingPolicy can still use its built-in model for the selected action.
    pub fn clear(&mut self) {
        self.script = None;
        self.attempted_source = None;
        self.attempted_stamp = None;
        self.load_error = None;
    }
    /// Reload a changed source. On failure the caller receives its provenance
    /// and script() is empty, so the public PacingPolicy uses the built-in model.
    pub fn reload(&mut self, source: &PolicySource, enabled: bool) -> Result<(), String> {
        if !enabled {
            self.clear();
            return Ok(());
        }
        let stamp = source
            .path()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
        if self.attempted_source.as_ref() == Some(source) && stamp == self.attempted_stamp {
            return self.load_error.clone().map_or(Ok(()), Err);
        }
        self.attempted_source = Some(source.clone());
        self.attempted_stamp = stamp;
        let label = source.label();
        let loaded = match source.path() {
            Some(path) => {
                std::fs::read_to_string(path).map_err(|error| format!("{label}: {error}"))
            }
            None => Ok(DEFAULT_SCRIPT.to_owned()),
        }
        .and_then(|text| {
            DelayScript::compile(&text, &label).map_err(|error| format!("{label}: {error}"))
        });
        match loaded {
            Ok(script) => {
                info!("loaded {label}");
                self.script = Some(script);
                self.load_error = None;
                Ok(())
            }
            Err(error) => {
                warn!("{error} — using built-in pacing model");
                self.script = None;
                self.load_error = Some(error.clone());
                Err(error)
            }
        }
    }
    pub fn script(&self) -> Option<&DelayScript> {
        self.script.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{BudgetSnapshot, DecisionKind, DelayInput};
    use super::*;
    use crate::config::{DelayModelConfig, InputConfig};

    fn base_input<'a>(cfg: &'a InputConfig, delay_cfg: &'a DelayModelConfig) -> DelayInput<'a> {
        DelayInput {
            kind: DecisionKind::Dahai,
            is_tsumogiri: false,
            is_post_call: false,
            first_action_of_kyoku: false,
            opening_animation: false,
            can_riichi: false,
            in_riichi: false,
            opponent_riichi: false,
            tile_class: None,
            junme: 0,
            legal_action_count: 1,
            probs: None,
            reading: Default::default(),
            budget: None,
            click_overhead_ms: 0,
            cfg,
            delay_cfg,
        }
    }

    #[test]
    fn script_result_is_used() {
        let s = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 2300 } end",
            "test",
        )
        .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let dec = s.try_decide(&base_input(&cfg, &d)).unwrap();
        assert_eq!(dec.total_target_ms, 2300);
        assert!(!dec.allow_bank);
    }

    #[test]
    fn script_sees_ctx_and_helpers() {
        let s = DelayScript::compile(
            r#"
            function decide_delay(ctx)
              assert(ctx.action == "dahai")
              assert(ctx.first_action == false)
              assert(ctx.legal_count == 1)
              assert(ctx.budget.fixed_ms == 5000)
              assert(ctx.budget.add_ms == 20000)
              local r = ctx.rng()
              assert(r >= 0 and r < 1)
              local ln = ctx.lognormal(0.6, 0.5)
              assert(ln > 0)
              return { delay_ms = 1000 + ctx.budget.elapsed_ms, allow_bank = true }
            end
            "#,
            "test",
        )
        .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let mut i = base_input(&cfg, &d);
        i.budget = Some(BudgetSnapshot {
            fixed_ms: 5000,
            add_ms: 20_000,
            elapsed_ms: 250,
        });
        let dec = s.try_decide(&i).unwrap();
        assert_eq!(dec.total_target_ms, 1250);
        assert!(dec.allow_bank);
    }

    /// What a script needs to pace a decision by how close it was: the
    /// spread of the bot's candidates in, and a wish for a look at the
    /// runner-up out.
    #[test]
    fn script_reads_the_spread_and_asks_for_a_look() {
        let s = DelayScript::compile(
            r#"
            function decide_delay(ctx)
              if ctx.entropy == nil then return { delay_ms = 1000 } end
              return { delay_ms = 1000 + 2000 * ctx.entropy, hesitate = ctx.margin < 0.2 }
            end
            "#,
            "test",
        )
        .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let mut i = base_input(&cfg, &d);
        // A bot that reports no probabilities: nothing to read, nothing asked.
        let dec = s.try_decide(&i).unwrap();
        assert_eq!((dec.total_target_ms, dec.hesitate), (1000, false));

        i.probs = Some(crate::autoplay::delay::DecisionProbs {
            top: 0.45,
            second: Some(0.40),
            entropy: 1.0,
        });
        let dec = s.try_decide(&i).unwrap();
        assert_eq!((dec.total_target_ms, dec.hesitate), (3000, true));

        i.probs = Some(crate::autoplay::delay::DecisionProbs {
            top: 0.97,
            second: Some(0.02),
            entropy: 0.25,
        });
        let dec = s.try_decide(&i).unwrap();
        assert_eq!((dec.total_target_ms, dec.hesitate), (1500, false));
    }

    /// The reading reaches a script as plain fields, and only the parts that
    /// were read: an unread fact is nil, never false.
    #[test]
    fn script_sees_the_reading_that_was_made() {
        let s = DelayScript::compile(
            r#"
            function decide_delay(ctx)
              if ctx.loose == nil then
                assert(ctx.runner_up_loose == nil and ctx.safe == nil and ctx.human_top == nil)
                return { delay_ms = 1000 }
              end
              assert(ctx.loose == true and ctx.runner_up_loose == false and ctx.safe == false)
              assert(ctx.human_top == 0.5 and ctx.human_second == 0.25 and ctx.human_chosen == 0.125)
              return { delay_ms = 2000 }
            end
            "#,
            "test",
        )
        .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let mut i = base_input(&cfg, &d);
        assert_eq!(s.try_decide(&i).unwrap().total_target_ms, 1000);
        i.reading = crate::autoplay::delay::Reading {
            loose: Some(true),
            runner_up_loose: Some(false),
            safe: Some(false),
            human: Some(crate::autoplay::delay::HumanView {
                top: 0.5,
                second: Some(0.25),
                chosen: 0.125,
            }),
        };
        assert_eq!(s.try_decide(&i).unwrap().total_target_ms, 2000);
    }

    #[test]
    fn a_script_may_say_how_long_a_look_rests() {
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let ask = |answer: &str| {
            let source = format!("function decide_delay(ctx) return {answer} end");
            let s = DelayScript::compile(&source, "test").unwrap();
            s.try_decide(&base_input(&cfg, &d))
        };
        assert_eq!(ask("{ delay_ms = 2300 }").unwrap().look_ms, None);
        assert_eq!(
            ask("{ delay_ms = 2300, look_ms = 1200 }").unwrap().look_ms,
            Some(1200)
        );
        assert_eq!(
            ask("{ delay_ms = 2300, look_ms = 950.5 }").unwrap().look_ms,
            Some(950)
        );
        assert!(ask("{ delay_ms = 2300, look_ms = 20 }").is_none());
        assert!(ask("{ delay_ms = 2300, look_ms = true }").is_none());
    }

    /// Lua's truthiness would read `hesitate = 0` as a yes.
    #[test]
    fn a_wish_that_is_not_a_boolean_is_a_script_error() {
        let s = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 2300, hesitate = 0 } end",
            "test",
        )
        .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        assert!(s.try_decide(&base_input(&cfg, &d)).is_none());
    }

    #[test]
    fn syntax_error_is_rejected_at_compile() {
        assert!(DelayScript::compile("function decide_delay(", "test").is_err());
        assert!(
            DelayScript::compile("x = 1", "test").is_err(),
            "no function"
        );
    }

    #[test]
    fn runtime_error_falls_back() {
        let s =
            DelayScript::compile("function decide_delay(ctx) error('boom') end", "test").unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        assert!(s.try_decide(&base_input(&cfg, &d)).is_none());
    }

    #[test]
    fn wrong_return_shape_falls_back() {
        for src in [
            "function decide_delay(ctx) return 42 end",
            "function decide_delay(ctx) return { } end",
            "function decide_delay(ctx) return { delay_ms = 'soon' } end",
            "function decide_delay(ctx) return { delay_ms = -5 } end",
            "function decide_delay(ctx) return { delay_ms = 1e12 } end",
            "function decide_delay(ctx) return { delay_ms = 0/0 } end",
        ] {
            let s = DelayScript::compile(src, "test").unwrap();
            let cfg = InputConfig::default();
            let d = DelayModelConfig::default();
            assert!(
                s.try_decide(&base_input(&cfg, &d)).is_none(),
                "must fall back for: {src}"
            );
        }
    }

    #[test]
    fn infinite_loop_is_aborted() {
        let s = DelayScript::compile("function decide_delay(ctx) while true do end end", "test")
            .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let start = Instant::now();
        assert!(s.try_decide(&base_input(&cfg, &d)).is_none());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "hook must abort the loop quickly"
        );
    }

    /// Regression: a `while true do end` at file scope used to hang
    /// `compile` forever (the guard hook was only installed for
    /// `decide_delay` calls, not for the chunk's top level).
    #[test]
    fn top_level_infinite_loop_is_rejected_at_compile() {
        let start = Instant::now();
        assert!(DelayScript::compile("while true do end", "test").is_err());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "compile must abort a top-level loop quickly"
        );
    }

    /// Regression: `pcall` used to be reachable and could catch the
    /// guard's abort error, making the instruction budget escapable.
    /// With `pcall` scrubbed, the shielded loop dies on the nil call.
    #[test]
    fn pcall_cannot_shield_a_runaway_loop() {
        let s = DelayScript::compile(
            r#"
            function decide_delay(ctx)
              while true do pcall(function() while true do end end) end
            end
            "#,
            "test",
        )
        .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let start = Instant::now();
        assert!(s.try_decide(&base_input(&cfg, &d)).is_none());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a pcall-shielded loop must still abort quickly"
        );
    }

    /// Regression: `load` accepted unvalidated precompiled bytecode
    /// (native-code execution) and `dofile()` blocked reading stdin.
    /// All loaders and `string.dump` must be gone; io/os/debug stay out.
    #[test]
    fn sandbox_scrubs_loaders_and_dump() {
        let s = DelayScript::compile(
            r#"
            assert(load == nil, "load must be nil")
            assert(loadstring == nil, "loadstring must be nil")
            assert(dofile == nil, "dofile must be nil")
            assert(loadfile == nil, "loadfile must be nil")
            assert(pcall == nil, "pcall must be nil")
            assert(xpcall == nil, "xpcall must be nil")
            assert(string.dump == nil, "string.dump must be nil")
            assert(os == nil and io == nil and debug == nil, "no io/os/debug")
            function decide_delay(ctx) return { delay_ms = 2000 } end
            "#,
            "test",
        )
        .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        assert!(s.try_decide(&base_input(&cfg, &d)).is_some());
    }

    /// Regression: with no memory limit, a huge `string.rep` ballooned
    /// RSS unchecked (C functions never tick the guard hook) and an
    /// allocation failure aborted the process. With the limit set it is
    /// a normal Lua error → fallback.
    #[test]
    fn allocation_bomb_falls_back() {
        let s = DelayScript::compile(
            r#"
            function decide_delay(ctx)
              local x = string.rep("x", 1e9)
              return { delay_ms = #x }
            end
            "#,
            "test",
        )
        .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let start = Instant::now();
        assert!(s.try_decide(&base_input(&cfg, &d)).is_none());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "allocation failure must be an immediate Lua error"
        );
    }

    /// Regression: `fixed_ms < 1000` used to drive the default script's
    /// `free_s` negative, producing a negative `delay_ms` that the host
    /// rejects — silently disabling the script for every decision in
    /// such a room.
    #[test]
    fn bundled_default_survives_tiny_fixed_budget() {
        let s = DelayScript::compile(DEFAULT_SCRIPT, "default").unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let mut i = base_input(&cfg, &d);
        i.budget = Some(BudgetSnapshot {
            fixed_ms: 500,
            add_ms: 1000,
            elapsed_ms: 0,
        });
        for _ in 0..200 {
            assert!(
                s.try_decide(&i).is_some(),
                "a sub-second fixed_ms must not reject the default script"
            );
        }
    }

    /// Regression: `allow_bank = 0` was read through Lua truthiness as
    /// `true`, silently unlocking bank spending. Non-boolean values are
    /// a script bug → fallback; nil stays "false".
    #[test]
    fn allow_bank_must_be_boolean() {
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let bad = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 2000, allow_bank = 0 } end",
            "test",
        )
        .unwrap();
        assert!(bad.try_decide(&base_input(&cfg, &d)).is_none());

        let good = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 2000, allow_bank = true } end",
            "test",
        )
        .unwrap();
        assert!(good.try_decide(&base_input(&cfg, &d)).unwrap().allow_bank);
    }

    /// The functional floor and budget cap bind the script's answer:
    /// returning 0 cannot click into the dealing animation, and a huge
    /// value cannot overrun the server window.
    #[test]
    fn script_cannot_break_floor_or_cap() {
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();

        let zero = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 0 } end",
            "test",
        )
        .unwrap();
        let mut i = base_input(&cfg, &d);
        i.opening_animation = true;
        let dec = zero.try_decide(&i).unwrap();
        assert_eq!(
            dec.total_target_ms, cfg.dealer_first_discard_extra_delay_ms,
            "animation floor must hold against a 0 return"
        );

        // Regression: even outside the opening animation, a 0 return is
        // lifted to the UI-readiness floor — clicking before Majsoul
        // renders the tiles loses the click.
        let dec = zero.try_decide(&base_input(&cfg, &d)).unwrap();
        assert_eq!(
            dec.total_target_ms, d.min_delay_ms,
            "min_delay_ms floor must hold against a 0 return"
        );

        // Regression: claim windows click an action button, which renders
        // later than tiles — the higher button floor must hold.
        let mut i = base_input(&cfg, &d);
        i.kind = DecisionKind::Pon;
        let dec = zero.try_decide(&i).unwrap();
        assert_eq!(
            dec.total_target_ms, d.min_button_delay_ms,
            "min_button_delay_ms floor must hold against a 0 return"
        );

        let huge = DelayScript::compile(
            "function decide_delay(ctx) return { delay_ms = 500000 } end",
            "test",
        )
        .unwrap();
        let mut i = base_input(&cfg, &d);
        i.budget = Some(BudgetSnapshot {
            fixed_ms: 5000,
            add_ms: 0,
            elapsed_ms: 0,
        });
        let dec = huge.try_decide(&i).unwrap();
        assert_eq!(
            dec.total_target_ms,
            5000 - d.safety_margin_ms,
            "soft cap must hold against a huge return"
        );
    }

    /// The bundled default (`DEFAULT_SCRIPT`, generated as `delay.lua`)
    /// must compile and produce human-plausible values.
    #[test]
    fn bundled_default_script_works() {
        let s = DelayScript::compile(DEFAULT_SCRIPT, "delay_default.lua").unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();

        // Plain tedashi: calibrated median ~2.4s. The script rng is not
        // seedable from here, so assert on a batch median with slack.
        let mut samples: Vec<u32> = (0..300)
            .map(|_| s.try_decide(&base_input(&cfg, &d)).unwrap().total_target_ms)
            .collect();
        samples.sort_unstable();
        let med = samples[samples.len() / 2];
        assert!(
            (1500..=4000).contains(&med),
            "default-script tedashi median {med} implausible"
        );
        // Floors/caps still bind: nothing below min_delay_ms, nothing
        // above the no-budget static cap.
        assert!(*samples.first().unwrap() >= d.min_delay_ms);
        assert!(*samples.last().unwrap() <= d.no_budget_cap_ms);

        // In riichi: measured Throne players still glance (~1.4s median),
        // never below the UI-readiness floor.
        let mut riichi_samples: Vec<u32> = (0..300)
            .map(|_| {
                let mut i = base_input(&cfg, &d);
                i.in_riichi = true;
                i.kind = DecisionKind::Pass;
                s.try_decide(&i).unwrap().total_target_ms
            })
            .collect();
        riichi_samples.sort_unstable();
        assert!(*riichi_samples.first().unwrap() >= d.min_delay_ms);
        let riichi_med = riichi_samples[riichi_samples.len() / 2];
        assert!(
            (1100..=1800).contains(&riichi_med),
            "in-riichi median {riichi_med} off calibration (~1.4s)"
        );

        // The default model ignores the bot's probabilities: a near-tie
        // must not shift the batch median (each batch re-rolls the rng,
        // so allow generous sampling slack).
        let mut tie_samples: Vec<u32> = (0..300)
            .map(|_| {
                let mut i = base_input(&cfg, &d);
                i.probs = Some(crate::autoplay::delay::DecisionProbs {
                    top: 0.40,
                    second: Some(0.399),
                    entropy: 0.0,
                });
                s.try_decide(&i).unwrap().total_target_ms
            })
            .collect();
        tie_samples.sort_unstable();
        let tie_med = tie_samples[tie_samples.len() / 2] as f64;
        let ratio = tie_med / med as f64;
        assert!(
            (0.8..=1.25).contains(&ratio),
            "near-tie median {tie_med} vs base {med} — probs must not shift the model"
        );

        // Claim windows are the fast reaction bucket: batch median well
        // under the tedashi one.
        let mut claims: Vec<u32> = (0..300)
            .map(|_| {
                let mut i = base_input(&cfg, &d);
                i.kind = DecisionKind::Pass;
                s.try_decide(&i).unwrap().total_target_ms
            })
            .collect();
        claims.sort_unstable();
        assert!(claims[claims.len() / 2] < med);
    }

    /// Regression: bots that report flat policy probabilities (top well
    /// under 0.60 on nearly every decision — e.g. the native bot's HUD
    /// probs) used to trip a `top_prob < 0.60 → routine × 0.3` rule in
    /// the bundled script, all but eliminating the fast "routine flick"
    /// cluster and inflating every discard. Flat probs must keep the
    /// same routine odds as no probs at all.
    #[test]
    fn bundled_default_flat_probs_keep_routine_fraction() {
        let s = DelayScript::compile(DEFAULT_SCRIPT, "delay_default.lua").unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();

        // Tsumogiri honor: the bucket with the largest routine weight
        // (0.15). With the collapse rule that dropped to 0.045, pushing
        // the fast (≤1.2s) fraction from ~30% down to ~24%.
        let fast_fraction = |probs: Option<crate::autoplay::delay::DecisionProbs>| {
            let n = 3000;
            let fast = (0..n)
                .filter(|_| {
                    let mut i = base_input(&cfg, &d);
                    i.is_tsumogiri = true;
                    i.tile_class = Some(crate::autoplay::delay::TileClass::Honor);
                    i.probs = probs;
                    s.try_decide(&i).unwrap().total_target_ms <= 1200
                })
                .count();
            fast as f64 / n as f64
        };

        // Flat probs with a tiny margin — the shape that used to trip
        // both removed rules (routine collapse and near-tie extra time).
        let flat = fast_fraction(Some(crate::autoplay::delay::DecisionProbs {
            top: 0.11,
            second: Some(0.105),
            entropy: 0.0,
        }));
        assert!(
            flat > 0.27,
            "flat-prob fast fraction {flat:.3} — routine collapse is back"
        );
    }

    #[test]
    fn host_hot_reloads_and_reports_missing_declared_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delay.lua");
        let mut host = ScriptHost::default();

        let error = host
            .reload(&PolicySource::Profile { path: path.clone() }, true)
            .unwrap_err();
        assert!(error.contains("profile pacing override"));
        assert!(!path.exists(), "loading must not generate a profile script");
        assert!(host.script().is_none());

        std::fs::write(
            &path,
            "function decide_delay(ctx) return { delay_ms = 1111 } end",
        )
        .unwrap();
        host.reload(&PolicySource::Profile { path: path.clone() }, true)
            .unwrap();
        assert!(host.script().is_some());

        // Rewrite with a different value (bump mtime explicitly — some
        // filesystems have coarse timestamps).
        std::fs::write(
            &path,
            "function decide_delay(ctx) return { delay_ms = 2222 } end",
        )
        .unwrap();
        let bumped = SystemTime::now() + Duration::from_secs(2);
        // Windows needs a handle with write access for SetFileTime; a
        // read-only `File::open` yields ERROR_ACCESS_DENIED.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(bumped).unwrap();
        host.reload(&PolicySource::Profile { path: path.clone() }, true)
            .unwrap();
        let cfg = InputConfig::default();
        let d = DelayModelConfig::default();
        let dec = host
            .script()
            .unwrap()
            .try_decide(&base_input(&cfg, &d))
            .unwrap();
        assert_eq!(dec.total_target_ms, 2222, "reload must pick up the edit");

        // Disabled: script dropped.
        host.reload(&PolicySource::Profile { path: path.clone() }, false)
            .unwrap();
        assert!(host.script().is_none());
    }

    #[test]
    fn source_switch_and_failure_cannot_reuse_a_previous_accounts_script() {
        let dir = tempfile::tempdir().unwrap();
        let bot = PolicySource::Bot {
            path: dir.path().join("bot.lua"),
        };
        std::fs::write(
            bot.path().unwrap(),
            "function decide_delay(ctx) return {delay_ms=1111} end",
        )
        .unwrap();
        let mut host = ScriptHost::default();
        host.reload(&bot, true).unwrap();
        assert!(host.script().is_some());
        let override_source = PolicySource::Profile {
            path: dir.path().join("missing.lua"),
        };
        let error = host.reload(&override_source, true).unwrap_err();
        assert!(host.script().is_none());
        assert_eq!(host.reload(&override_source, true).unwrap_err(), error);
        std::fs::write(override_source.path().unwrap(), "function decide_delay(").unwrap();
        assert!(host
            .reload(&override_source, true)
            .unwrap_err()
            .contains("profile pacing override"));
        assert!(host.script().is_none());
        host.reload(&PolicySource::Bundled, true).unwrap();
        assert!(host.script().is_some());
        assert_eq!(
            std::fs::read_to_string(bot.path().unwrap()).unwrap(),
            "function decide_delay(ctx) return {delay_ms=1111} end"
        );
    }
}
