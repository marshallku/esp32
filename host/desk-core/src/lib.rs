//! Everything about the desk panel that is not hardware: the wire protocol,
//! the screen geometry, gesture recognition and the device's UI state machine.
//!
//! It is shared by the firmware and the PC daemon so the two cannot disagree —
//! the daemon decides whether a prompt fits using the same `wrap` the panel
//! draws with — and so that the logic is unit-tested on the PC rather than
//! only ever exercised by poking a screen.

#![no_std]

extern crate alloc;

pub mod app;
pub mod geometry;
pub mod gesture;
pub mod icons;
pub mod protocol;
pub mod text;
