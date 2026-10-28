//! One effective input profile for a real decision window. Inference reserves
//! and physical execution resolve the same profile, without changing settings.
use crate::config::InputConfig;

pub(crate) struct InputTiming {
    pub input: InputConfig,
}

impl InputTiming {
    pub fn for_clock(configured: &InputConfig, fixed_ms: Option<u32>) -> Self {
        let mut input = configured.clone();
        // Short clocks are normalized facts, shared by both browser clients.
        // Unknown timing must not invent a faster room or additional bank.
        if matches!(fixed_ms, Some(1..=3_000)) {
            if input.hover_delay_ms > 0 {
                input.hover_delay_ms = input
                    .hover_delay_ms
                    .div_ceil(2)
                    .max(super::humanize::MIN_HOVER_MS);
            }
            input.click_hold_ms = input.click_hold_ms.div_ceil(2);
            input.inter_click_delay_ms = input.inter_click_delay_ms.div_ceil(2);
        }
        Self { input }
    }

    pub fn inference_reserve_ms(&self, safety_margin_ms: u32) -> u32 {
        // Keep network/scheduling margin and pointer travel intact. Only the
        // configured hover, hold and staged click gaps have been accelerated.
        safety_margin_ms
            .saturating_add(
                self.input
                    .hover_delay_ms
                    .saturating_add(self.input.click_hold_ms)
                    .saturating_mul(3),
            )
            .saturating_add(self.input.inter_click_delay_ms.saturating_mul(2))
            .saturating_add(600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoplay::{humanize, BudgetSource, TimeBudget};
    use crate::bot::control::InputWindow;
    use rand::{rngs::StdRng, SeedableRng};
    use std::time::Instant;

    #[test]
    fn short_clocks_on_either_client_resolve_the_same_real_input_profile() {
        let configured = InputConfig::default();
        let now = Instant::now();
        let windows = [
            InputWindow::Majsoul(TimeBudget {
                fixed_ms: 3_000,
                add_ms: 20_000,
                opened_at: now,
                source: BudgetSource::DealTile,
            }),
            InputWindow::Tenhou(crate::autoplay::tenhou_state::DecisionWindow {
                ops: 0,
                opened_at: now,
                clock: Some(crate::autoplay::tenhou_state::TurnClock::FAST),
            }),
        ];
        for window in windows {
            let timing = InputTiming::for_clock(&configured, window.fixed_ms());
            assert_eq!(timing.input.hover_delay_ms, 100);
            assert_eq!(timing.input.click_hold_ms, 50);
            assert_eq!(timing.input.inter_click_delay_ms, 150);
            assert_eq!(timing.inference_reserve_ms(1_000), 2_350);
            // Verification remains evidence-driven, never sped up into retries.
            assert_eq!(timing.input.verify_input_ms, configured.verify_input_ms);
            assert_eq!(timing.input.click_retries, configured.click_retries);
            assert_eq!(
                timing.input.reload_after_failures,
                configured.reload_after_failures
            );
        }
        assert_eq!(configured.hover_delay_ms, 200, "settings were not mutated");
    }

    #[test]
    fn normal_unknown_and_zero_base_clocks_keep_the_configured_profile() {
        let configured = InputConfig::default();
        for fixed in [Some(5_000), None, Some(0)] {
            let timing = InputTiming::for_clock(&configured, fixed);
            assert_eq!(timing.input.hover_delay_ms, 200);
            assert_eq!(timing.input.click_hold_ms, 100);
            assert_eq!(timing.input.inter_click_delay_ms, 300);
            assert_eq!(timing.inference_reserve_ms(1_000), 3_100);
        }
    }

    #[test]
    fn accelerated_hovers_still_survive_the_clients_input_sampling() {
        let configured = InputConfig::default();
        let timing = InputTiming::for_clock(&configured, Some(3_000));
        let mut rng = StdRng::seed_from_u64(31);
        for _ in 0..2_000 {
            assert!(humanize::HOVER.draw_ms(timing.input.hover_delay_ms, &mut rng) >= 100);
            assert!(humanize::HOLD.draw_ms(timing.input.click_hold_ms, &mut rng) > 0);
        }
    }
}
