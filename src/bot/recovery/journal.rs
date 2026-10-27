//! The one confirmed-event ledger. Rejoin frames are observations of a
//! prefix, not new moves. Neither worker can append or rewrite this ledger.

use crate::monitor::DecisionContext;
use crate::schema::MjaiEvent;
use anyhow::{bail, Result};
use std::sync::{Arc, Mutex};

pub type SharedJournal = Arc<Mutex<Vec<MjaiEvent>>>;

enum View {
    Live,
    Replaying { next: usize, end: usize },
}

pub struct Journal {
    pub events: SharedJournal,
    last: Option<(DecisionContext, MjaiEvent)>,
    view: View,
}

pub enum Delivery {
    Duplicate,
    Observe {
        event: Option<MjaiEvent>,
        through: u64,
        frontier: bool,
    },
}

impl Journal {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            last: None,
            view: View::Live,
        }
    }

    pub fn ingest(&mut self, context: DecisionContext, event: MjaiEvent) -> Result<Delivery> {
        if let Some((previous, previous_event)) = &self.last {
            if context == *previous && event == *previous_event {
                return Ok(Delivery::Duplicate);
            }
            if context.event_seq != previous.event_seq + 1 {
                bail!(
                    "Non-contiguous tracked events: {} after {}",
                    context.event_seq,
                    previous.event_seq
                );
            }
        } else if !matches!(event, MjaiEvent::StartGame { .. }) {
            bail!("A recovery journal must begin at a real start_game");
        }
        self.last = Some((context, event.clone()));
        let mut events = self.events.lock().expect("journal poisoned");
        if matches!(event, MjaiEvent::StartGame { .. }) && !events.is_empty() {
            // BotManager admits this path only for the same provider game id,
            // seat and player count. It never guesses identity from a hand.
            let start = events
                .iter()
                .rposition(|e| matches!(e, MjaiEvent::StartKyoku { .. }))
                .ok_or_else(|| anyhow::anyhow!("Rejoin has no known hand prefix"))?;
            self.view = View::Replaying {
                next: start,
                end: events.len(),
            };
            return Ok(Delivery::Observe {
                event: None,
                through: events.len() as u64,
                frontier: false,
            });
        }
        if let View::Replaying { next, end } = &mut self.view {
            if events.get(*next) != Some(&event) {
                bail!(
                    "Rejoin differs from confirmed event {}. No event or cache is skipped.",
                    next
                );
            }
            *next += 1;
            let frontier = *next == *end;
            if frontier {
                self.view = View::Live;
            }
            return Ok(Delivery::Observe {
                event: None,
                through: events.len() as u64,
                frontier,
            });
        }
        events.push(event.clone());
        Ok(Delivery::Observe {
            event: Some(event),
            through: events.len() as u64,
            frontier: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(game: u64, seq: u64) -> DecisionContext {
        DecisionContext {
            game,
            round: 1,
            event_seq: seq,
        }
    }
    fn event(json: &str) -> MjaiEvent {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn duplicate_and_rejoin_frames_do_not_apply_real_actions_twice() {
        let start = event(r#"{"type":"start_game","id":0,"names":[]}"#);
        let deal = event(
            r#"{"type":"start_kyoku","bakaze":"E","kyoku":1,"honba":0,"kyotaku":0,"oya":0,"dora_marker":"C","scores":[25000,25000,25000,25000],"tehais":[[],[],[],[]]}"#,
        );
        let draw = event(r#"{"type":"tsumo","actor":0,"pai":"N"}"#);
        let mut journal = Journal::new();
        journal.ingest(context(1, 9), start.clone()).unwrap();
        journal.ingest(context(1, 10), deal.clone()).unwrap();
        journal.ingest(context(1, 11), draw.clone()).unwrap();
        assert!(matches!(
            journal.ingest(context(1, 11), draw.clone()).unwrap(),
            Delivery::Duplicate
        ));
        for (seq, e) in [(12, start), (13, deal), (14, draw)] {
            assert!(matches!(
                journal.ingest(context(2, seq), e).unwrap(),
                Delivery::Observe {
                    event: None,
                    through: 3,
                    ..
                }
            ));
        }
        let discard = event(r#"{"type":"dahai","actor":0,"pai":"N","tsumogiri":true}"#);
        assert!(matches!(
            journal.ingest(context(2, 15), discard).unwrap(),
            Delivery::Observe {
                event: Some(_),
                through: 4,
                ..
            }
        ));
        assert_eq!(journal.events.lock().unwrap().len(), 4);
        assert!(journal.ingest(context(2, 17), MjaiEvent::None).is_err());
    }
}
