# Tailout application icon

Artwork was created with the built-in image-generation tool. The master is preserved as generated; the PNG sizes and ICO are mechanical resamples and format packaging.

## Files

- `tailout-master.png`: full-resolution corrected artwork.
- `tailout.ico`: 32-bit RGBA Windows icon with 16, 20, 24, 32, 40, 48, 64, 96, 128 and 256 px images.
- `tailout-{size}.png`: individual images at those sizes.
- `tailout-512.png`: larger PNG for documentation and reuse.
- `tailout.rc`: Windows executable icon resource.
- `tailout-sizes-preview.png`: light/dark readability proof, including enlarged 16 px pixels.

The charcoal badge is opaque. Only its outer rounded corners/background are transparent.

## Rebuild the icon files

Requires Python 3 and FFmpeg; no Python imaging packages are needed.

```powershell
python assets/generate_icons.py
```

The Windows build compiles `tailout.rc` automatically. MSVC builds discover the Windows SDK resource compiler; LLVM's `llvm-rc` is a fallback. GNU builds use `windres`. Set `RC` to override the resource compiler.

## Current design (2026-10-20)

Created with the built-in image-generation tool, then corrected with the same tool.
A large amber-gold reel is the main subject; a curled film strip emerges from its
bottom edge with three ivory picture frames and two rows of rectangular sprocket
holes. Small ivory scissors form a secondary accent. The badge interior is opaque.

### Generation prompt

Use case: logo-brand.
Asset type: finished square Windows desktop program icon for Tailout, a video trimming application, readable at 16x16 through 256x256.
Primary request: a prominent classic movie film reel with film emerging from its bottom edge, and a much smaller pair of scissors as a secondary accent. This is a fresh icon design.
Composition: one opaque charcoal rounded-square badge with transparent exterior corners. A large warm amber-gold front-facing film reel dominates the upper two-thirds, taking roughly 70% of the icon width. It has a thick circular rim, three or four large dark circular reel openings and a simple hub, immediately recognizable as a classic cinema reel. From the BOTTOM side of the reel an attached strip of classic motion-picture film unspools downward and bends across the lower part of the badge in a gentle broad curl. Keep the strip wide enough to show two or three unmistakable rectangular picture frames ("slides"), separated by substantial crossbars, with a row of clearly defined rectangular sprocket holes along EACH long edge of the film. The framed images are empty pale ivory rectangles, the film body is dark with bold amber/ivory edge structure, and the sprocket holes show the charcoal backing, all fully opaque within the badge. Make this look like real old-fashioned perforated 35mm film, with its frames and BOTH rows of perforations visible and distinct. One compact ivory pair of scissors with two clear handle loops and short blades occupies a small corner beside the film, only about a quarter of the badge width, much smaller than the reel, without covering the reel or film frames.
Style: crisp geometric, clean flat vector-like raster artwork, confident thick shapes and generous spacing, very limited palette of charcoal, warm amber-gold and ivory. Subtle polish, no textures or fine hairline details. Recognizable silhouette and strong light/dark contrast at tiny Windows taskbar size.
Constraints: one single finished icon, square canvas, centered badge with comfortable outer margin. No text, letters, numbers, labels, watermark, mockup, presentation grid, shadow cast outside the badge, realistic photography, green reel, film emerging from the top, large dominant scissors or extra objects. The badge interior is entirely opaque; reel holes, sprocket holes and scissors loops reveal opaque charcoal, never accidental transparency. Transparency ONLY outside the rounded square.

### Corrective edit prompt

Use case: precise-object-edit. Edit this newly generated Tailout icon. Change ONLY the accidental irregular black/transparent vertical blemish in the upper-right of the charcoal rounded-square backing, above the small scissors and to the right of the large gold reel. Completely remove this blemish and smoothly fill its whole area with the surrounding matching opaque charcoal. The background inside the badge should be a clean, continuous, fully opaque charcoal surface with no holes, no smudges, no cutouts, no seams. All pixels inside the rounded-square badge are opaque, including dark areas in reel holes, film sprocket holes and scissors handles. Transparency belongs ONLY to the exterior outside the rounded-square silhouette, with antialiasing on the outer boundary only. Preserve the large amber-gold reel, the three ivory film picture frames, the two rows of rectangular film sprocket holes, the curled film emerging from the bottom of the reel, the small ivory scissors, all sizes, positions and every other visual detail unchanged. Do not redesign, crop or add anything.


## Previous icon generation prompt

Use case: logo-brand.
Asset type: finished square Windows desktop application icon for Tailout, a minimal video trimming and HEVC conversion application.
Primary request: one distinctive, extremely simple film reel and scissors emblem, designed to remain recognizable at 16 by 16 pixels.
Composition: a dark charcoal rounded-square badge fills about 92% of the square canvas, with real transparent pixels outside its rounded corners. On the badge, one large mint-green movie film reel in the upper-left, with only three large dark circular reel holes, combined with one bold ivory pair of open scissors across the lower-right quarter. The scissors should have two unmistakable large handle loops and short thick crossed blades, aimed toward the reel. Integrate these into one balanced, compact silhouette with generous clear gaps and very few shapes.
Style: precise flat vector-like raster icon, crisp geometric edges, front-facing, bold solid fills. Use the existing application's restrained mint-green, charcoal and ivory visual language. Clear contrast on both light and dark Windows taskbars.
Constraints: single icon only, centered, square image, transparent outside the badge. No text, letters, tiny details, perforated filmstrips, small bolts, fine strokes, outlines, gradients, shadows, 3D rendering, mockups, sample grids or backgrounds. Small-size readability is more important than realism.

## Corrective edit prompt

Use case: precise-object-edit. Edit the provided Tailout application icon. Fix the unwanted small transparent hole / blemish in the upper-right interior of the dark rounded-square badge. The badge must be one continuous solid opaque charcoal backing: all interior pixels, including the dark reel holes, scissors handle openings and hinge, must remain fully opaque. Transparency belongs ONLY outside the outer rounded-square silhouette, with antialiasing only along that outer edge. Fill the accidental upper-right interior hole with the matching charcoal background, and remove any other accidental interior transparency. Preserve the existing reel-and-scissors design, geometry, positions, scale, colors, proportions and transparent outer corners. Do not redesign the icon, add details, add text or change the silhouette.
