//! Plumb as a standalone VST3 plugin (`Plumb.vst3`), part of the Bent Signal bundle: the
//! `soundcraft-dsp` Plumb monophonic pitch correction behind the VST3 COM interfaces, with the
//! shared Bent Signal editor (`bentsignal-ui`).
//!
//! - One audio module class ("Plumb", `Fx|Pitch Shift`), a single-component plugin: the component
//!   is also its edit controller. Stereo in, stereo out, 32-bit float.
//! - Latency: 20 ms, or in Lock (Retune Speed below 0) 45 ms plus the lookahead, or in Record
//!   mode 0.5 ms; a change is reported with `restartComponent(kLatencyChanged)`. Bypass is
//!   delayed to match.
//! - Parameters (stable ids): 0 Key, 1 Scale, 2 Retune Speed, 3 Flex Tune, 4 Reference, 5 Mode,
//!   6 Bypass, 7 Link (Off or groups 1..=5: instances in one group share ids 0..=5, see `sync`).
//!   Normalized values map through the DSP's own `ParamInfo` ranges. Changes are applied
//!   sample-accurately.
//! - State: `PLMB`, version, count, plain values, the editor size (see `params`).
//! - Editor (Windows): the shared Bent Signal editor, described by `ui::SPEC`.
//!
//! Like `soundcraft-vst3-host`, this is an isolated `unsafe` crate (craftrules never-crash):
//! `unsafe` is denied crate-wide and allowed only in the COM module (`plugin`); the Win32 editor
//! lives in `bentsignal-ui`. Nothing panics outside tests.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// The crate's library name is the bundle name ("Plumb"); VST3 exports are CamelCase.
#![allow(non_snake_case)]
// The VST3 binding constants' integer types differ between platforms, so some casts are no-ops here.
#![allow(clippy::unnecessary_cast)]

pub mod params;
pub mod sync;
pub mod ui;

#[allow(unsafe_code)]
pub mod plugin;

pub use plugin::{GetPluginFactory, PLUMB_CID};

#[cfg(test)]
#[allow(unsafe_code, clippy::unwrap_used, clippy::panic)]
mod tests;
