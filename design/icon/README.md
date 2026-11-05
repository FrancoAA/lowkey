# App icon

`icon-graphite.svg` is the app icon: five rounded off-white bars (#f4f4f5), symmetric, tallest
in the centre, on a charcoal rounded square (#27272a to #141416, radius 230 of 1024, a thin
#3f3f46 edge so it stays visible on dark panels). `icon-graphite.png` is its 1024 px render.

- Drawn by hand as SVG on 2026-11-03, after Franco asked for a more sober icon than the
  colourful one below. Chosen from four candidates (`compare-sober.png`: each at 256, 64, 32
  and 16 px, on a dark and on a light background) because it reads best at every size on both.
  The others: the same bars dark on paper (weak on light backgrounds), the same with a soft
  violet centre bar, and a fal.ai FLUX image with dots that vanish at 32 px.
- The icon set in `desktop/src-tauri/icons/` comes from the PNG:
  `cd desktop && ../frontend/node_modules/.bin/tauri icon ../design/icon/icon-graphite.png -o src-tauri/icons`
  (then delete the `android/` and `ios/` folders it also writes). The page and the README use
  a 256 px render (`frontend/public/icon.png`, `assets/icon.png`):
  `rsvg-convert -w 256 -h 256 design/icon/icon-graphite.svg -o frontend/public/icon.png`.

## Before (2026-11-02 to 2026-11-03)

`candidate-glow-bars.png`: five glowing bars, magenta to cyan, on a dark violet squircle.

- Made with fal.ai, model `fal-ai/flux-pro/v1.1-ultra` (aspect 1:1). The raw output is
  `source-glow-bars-flux.png`; the icon was its centre 93%, resized to 1024 px, with a
  rounded-square mask (radius 230 px, ImageMagick).
- Prompt: "Full-bleed square app icon artwork, edge to edge, no margins, no frame. Background:
  smooth deep plum to near-black gradient (#2a0b33 to #0f0f12). Centered: a compact symbol of
  7 rounded vertical bars forming a sound wave, tallest in the center; each bar is split by a
  narrow horizontal gap at the middle: the upper part glowing magenta (#e879f9), the lower
  part glowing cyan (#22d3ee), like a voice and its answer. Soft subtle glow, flat crisp
  shapes, minimal, lots of empty space around the symbol. No text, no letters, no circle, no
  disc."
- It was chosen over `candidate-mirror-wave.png` (Ideogram V3) because it read at 32 px
  (`compare-sizes.png`).
