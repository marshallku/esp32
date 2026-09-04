//! The `/homelab.json` document, as produced by the `homelab-status` stack on
//! pi01 (see `marshallku/manifest`, `docker-compose/pi01/homelab-status`).
//!
//! The capacities below are not arbitrary: they mirror `MAX_GROUPS`,
//! `MAX_DOWN`, `MAX_HOSTS` and `MAX_NAME_LEN` in that stack's `aggregate.py`.
//! `serde-json-core` fails a parse outright when an array overruns its
//! `heapless::Vec`, so the two sides have to be changed together — the
//! aggregator's README says the same thing from the other end.

use heapless::{String, Vec};
use serde::Deserialize;

pub const MAX_GROUPS: usize = 10;
pub const MAX_DOWN: usize = 6;
pub const MAX_HOSTS: usize = 6;

/// A monitor group rolled up to "n of m up".
#[derive(Debug, Deserialize)]
pub struct Group {
    pub label: String<16>,
    pub up: u16,
    pub total: u16,
}

/// A monitor that is currently down, and for how long.
#[derive(Debug, Deserialize)]
pub struct Down {
    pub name: String<32>,
    /// `None` when Kuma has no recorded state change to measure from.
    pub secs: Option<u32>,
}

/// Uptime Kuma's half of the document. Survives a prd01 outage.
#[derive(Debug, Deserialize)]
pub struct Kuma {
    pub ok: bool,
    pub up: u16,
    /// Settled monitors only — see `unsettled`.
    pub total: u16,
    /// Monitors mid-retry (PENDING) or in maintenance. They are deliberately
    /// outside `up`/`total` so the headline ratio does not flap during a retry
    /// window, which means this count is the only place they appear. Dropping
    /// it from the screen would quietly shrink the denominator.
    pub unsettled: u16,
    pub groups: Vec<Group, MAX_GROUPS>,
    pub down: Vec<Down, MAX_DOWN>,
    /// Down monitors the aggregator had to trim to fit the screen.
    pub down_more: u16,
}

/// Per-node gauges. All optional: a node missing one series is drawn blank
/// rather than as a confident zero.
#[derive(Debug, Deserialize)]
pub struct Node {
    pub name: String<16>,
    pub cpu: Option<f32>,
    pub mem: Option<f32>,
    pub disk: Option<f32>,
    pub load: Option<f32>,
    pub up_d: Option<f32>,
}

/// Prometheus' half. `ok` goes false when the cluster it lives in is gone.
#[derive(Debug, Deserialize)]
pub struct Hosts {
    pub ok: bool,
    pub nodes: Vec<Node, MAX_HOSTS>,
}

/// The whole document.
#[derive(Debug, Deserialize)]
pub struct Status {
    /// Seconds since the snapshot was built, stamped when the request was
    /// served. The board has no clock, so this is the only way it can know
    /// how old the data is.
    pub age: i32,
    pub stale: bool,
    /// False before the aggregator's first refresh has landed.
    pub ready: bool,
    pub kuma: Kuma,
    pub hosts: Hosts,
}

impl Status {
    /// Parse a response body. Trailing bytes after the object are ignored.
    pub fn parse(body: &[u8]) -> Result<Self, serde_json_core::de::Error> {
        serde_json_core::from_slice::<Status>(body).map(|(status, _)| status)
    }
}
