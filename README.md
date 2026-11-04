# Shifter VST3

Stereo micro-pitch doubler as a standalone VST3 plugin (Windows x64), with its own Win32 editor. Rust; DSP from the `soundcraft-dsp` crate.

![Shifter editor](docs/shifter-ui.png)

## Modes
- **Twin**: static doubler. Left voice tuned up, right voice down, right voice slightly earlier.
- **Drift**: Twin with a slow LFO nudging detune and delay, like a gentle chorus.
- **Spread**: mid/side. Voice difference is added to the sides only; centre stays dry, effect vanishes in mono.

## Controls
- Mode: Twin / Drift / Spread
- Delay (ms): offset of the detuned voices behind the dry signal
- Detune (ct): pitch offset per voice
- Anchor (Hz): content below this stays centred and undetuned
- Width (%): stereo separation of the two voices
- Mix (%): dry/effect balance
- Output (dB): final gain, -24 to +12
- Phase Swap: flips polarity of the right voice (side signal in Spread)
- Bypass: click-free dry pass-through
- Presets: Default, Gentle Double, Wide Twin, Slow Drift, Mono-Safe Spread, Hollow Swap

## Build
Needs Rust (MSVC toolchain, 1.90+).

    $env:CARGO_TARGET_DIR = "D:\ai\Projects\bt-shifter-build"
    cargo build --release -p soundcraft-shifter-vst3 --lib
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

Installer (needs WiX v5: `dotnet tool install --global wix --version 5.0.2`):

    powershell -ExecutionPolicy Bypass -File scripts\package.ps1

Output: `dist\Shifter-VST3-0.1.0-x64.msi`.

## Install
The MSI (per-machine, needs admin) installs the bundle to `C:\Program Files\Common Files\VST3\Shifter.vst3`:

    Shifter.vst3\Contents\x86_64-win\Shifter.vst3
    Shifter.vst3\Contents\Resources\

Or copy the bundle there by hand.

## License
All rights reserved.
