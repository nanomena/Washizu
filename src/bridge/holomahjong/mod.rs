//! HoloMahjong protocol bridge. Observe-only.
//!
//! Placeholder: reads nothing yet.

use super::{Bridge, Direction, ParseResult};
use crate::{
    logger::{FlowLogger, Session},
    schema::MjaiEvent,
};
use std::sync::Arc;

pub struct HoloMahjongBridge {
    _flow_log: Option<Arc<FlowLogger>>,
    _session: Option<Arc<Session>>,
}

impl HoloMahjongBridge {
    pub fn new(flow_log: Option<Arc<FlowLogger>>, session: Option<Arc<Session>>) -> Self {
        Self {
            _flow_log: flow_log,
            _session: session,
        }
    }
}

impl Bridge for HoloMahjongBridge {
    fn parse(&mut self, _direction: Direction, _content: &[u8]) -> ParseResult {
        ParseResult::default()
    }

    fn build(&mut self, _command: &MjaiEvent) -> Option<Vec<u8>> {
        None
    }
}
