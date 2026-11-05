# Bent Signal house style

One editor for every plugin in the bundle: `crates/bentsignal-ui`. A plugin supplies a `UiSpec`
(`src/ui.rs` in its crate: knobs, switches, one selector, optional meter, presets, a `ParamModel`)
and gets the same drawing, input, tooltips and window chrome. Nothing here imitates another
company's look; it is Shifter's editor, generalised.

## Grid
- Base canvas 640 wide; each plugin picks the height (Shifter 330, Grit 420). Everything scales
  uniformly and is centred. Minimum 75% of base, maximum 300%, drawn at 2x supersampling.
- Header: preset bar 0-32 (prev/next 10-54, preset name 60-300, save 306, reset 330, bundle name
  right-aligned ending at 550, BYPASS 560-630). Below it a 6 px walnut strip, then the title row.
- Title row y 38-70: wordmark centred (plugin name, upper case, 27 px, letter-spaced) with a 2 px
  rust line either side (x 20-236, 404-620). Walnut veneer also frames both sides (8 px) and the bottom (14 px).
- Large knob (r 30): caption above (cy-58), dial, end labels (cy+34, half-width rad+22 unless
  `end_span` is set), value (cy+50). Small knob (r 17): caption left, value right, ends below.
  Row pitch is 102-120 px; tight rows set `end_span: 40`.
- Switch: label above (cy-31), 22 px box, ON/OFF text below (cy+15). Selector: marks above
  (cy-35), 30 px buttons on a `spacing` pitch, caption (cy+20), selected name (cy+36).
- Meter: 10 px bar, caption left, dB readout right.

## Palette tokens (RGB hex)
panel 1C1917, bar 27231F, edge 0D0B0A, field 12100F, button 3A3531, button edge 58514A,
rust (accent) B85C32, rust dim 6B3B24, text DAD2C6, dim 8E867C, dial 2C2825, track 3E3833,
pointer EEE8DE, tooltip bg 0F0D0C. One accent only; everything else is warm charcoal.

## Type (Bahnschrift, px on the base canvas)
wordmark 27 semibold; captions and names 12.5 semibold; end labels, values, switch labels,
bundle name, buttons 10.5 regular; preset name 13 regular. Captions upper case.

## Controls
- Knob: 270 degree sweep from bottom-left. Drag vertically, 220 px for the full range; Shift
  is 5x finer; double-click or Ctrl-click resets to default. A rust arc shows the value; a
  bipolar knob (`origin`) draws from its centre. Hover brightens the value text.
- Switch: click toggles; lamp (square) or glyph, rust when on. Selector: exclusive buttons, rust
  edge and lit lamp on the active one. Bypass: rust fill when on; knob arcs dim.
- Meter: segmented bar (4 px segments), rust normally; a marker line shows a threshold and
  segments past it turn pointer-white. Level falls 20 dB/s; floor -48 dB, ceiling 0 dB.
- Presets: bar menu, arrows step, Save keeps a session-only "User" slot, Reset loads preset 1.

## Tooltips
Appear after about 0.45 s of hovering, 260 px wide, word-wrapped, dark box with a dim rust
border, placed below-right of the pointer and flipped when near an edge. One or two sentences:
what it does, then the useful range or effect. Every control and the meter has one; no
marketing words, no product names.

## Header and naming
Wordmark is the plugin name; "BENT SIGNAL" (dim, small, letter-spaced) sits in the preset bar.
Window class `BentSignal<Name>Editor`. Vendor in the VST3 factory stays as the project sets it.
