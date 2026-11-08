//! Grit as a standalone VST3 plugin (`Grit.vst3`), part of the Bent Signal bundle: the
//! `soundcraft-dsp` Grit multi-style saturation behind the VST3 COM interfaces, with the shared Bent Signal editor (`bentsignal-ui`).
//!
//! - One audio module class ("Grit", `Fx|Distortion`), a single-component plugin: the component is
//!   also its edit controller. Stereo in, stereo out, 32-bit float, latency
//!   `soundcraft_dsp::plugins::grit::latency_samples()` (the 2x oversampling half-band).
//! - Parameters (stable ids): 0 Style, 1 Low Cut, 2 Thump, 3 Tone, 4 Drive, 5 Punish, 6 High Cut,
//!   7 Steep, 8 Output, 9 Auto, 10 Mix, 11 Bypass. Normalized values map through the DSP's own
//!   `ParamInfo` ranges. Changes are applied sample-accurately.
//! - State: `GRIT`, version, count, plain values (see `params`).
//! - Editor (Windows): the shared Bent Signal editor, described by `ui::SPEC`.
//!
//! Like `soundcraft-vst3-host`, this is an isolated `unsafe` crate (craftrules never-crash):
//! `unsafe` is denied crate-wide and allowed only in the COM module (`plugin`); the Win32 editor
//! lives in `bentsignal-ui`. Nothing panics outside tests.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// The crate's library name is the bundle name ("Grit"); VST3 exports are CamelCase.
#![allow(non_snake_case)]
// The VST3 binding constants' integer types differ between platforms, so some casts are no-ops here.
#![allow(clippy::unnecessary_cast)]

pub mod params;
pub mod ui;

#[allow(unsafe_code)]
pub mod plugin;

pub use plugin::{GetPluginFactory, GRIT_CID};

#[cfg(test)]
#[allow(unsafe_code, clippy::unwrap_used, clippy::panic)]
mod tests;
