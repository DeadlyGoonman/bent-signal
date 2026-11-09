//! The Bent Signal house UI. Every plugin in the bundle describes itself with a `UiSpec` (knobs,
//! switches, one selector, an optional meter, presets, a `ParamModel`) and gets the same editor:
//! layout grid, palette, fonts, widgets, tooltips and window chrome. See `docs/house-style.md`.
//!
//! - `spec`: the description a plugin supplies. `shared`: values shared by processor, controller
//!   and editor. `layout`: platform-free geometry and hit testing. `paint`: pure shading.
//! - `host`: edits to the host's `IComponentHandler`. `editor`: the Win32 `IPlugView`.
//!
//! `unsafe` is denied crate-wide and allowed only in `host`, `editor` and `update_win`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// The VST3 binding constants' integer types differ between platforms, so some casts are no-ops here.
#![allow(clippy::unnecessary_cast)]

pub mod layout;
pub mod paint;
pub mod shared;
pub mod spec;
pub mod update;

#[allow(unsafe_code)]
pub mod host;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod editor;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod update_win;

pub use shared::Shared;
pub use spec::*;
