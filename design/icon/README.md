# App icon

`candidate-glow-bars.png` (1024 px, transparent corners) is the app icon: five rounded bars,
magenta to cyan (the colours of Me and Them in the app), on a dark violet squircle.

- Made on 2026-11-02 with fal.ai, model `fal-ai/flux-pro/v1.1-ultra` (aspect 1:1). The raw
  output is `source-glow-bars-flux.png`; the icon is its centre 93%, resized to 1024 px, with
  a rounded-square mask (radius 230 px) applied here (ImageMagick).
- Prompt: "Full-bleed square app icon artwork, edge to edge, no margins, no frame. Background:
  smooth deep plum to near-black gradient (#2a0b33 to #0f0f12). Centered: a compact symbol of
  7 rounded vertical bars forming a sound wave, tallest in the center; each bar is split by a
  narrow horizontal gap at the middle: the upper part glowing magenta (#e879f9), the lower
  part glowing cyan (#22d3ee), like a voice and its answer. Soft subtle glow, flat crisp
  shapes, minimal, lots of empty space around the symbol. No text, no letters, no circle, no
  disc."
- Chosen over `candidate-mirror-wave.png` (Ideogram V3) because it still reads at 32 px
  (`compare-sizes.png`: 512, 64 and 32 px).
- The icon set in `desktop/src-tauri/icons/` comes from it: `tauri icon
  design/icon/candidate-glow-bars.png`. The page and the README use a 256 px copy
  (`frontend/public/icon.png`, `assets/icon.png`).
