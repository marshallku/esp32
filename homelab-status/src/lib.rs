//! The `/homelab.json` status document served by the `homelab-status` stack
//! on pi01, and the WiFi + HTTP plumbing to fetch it.

#![no_std]

pub mod model;
pub mod net;
