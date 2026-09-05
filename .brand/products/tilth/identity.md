# tilth — product identity

A product layer over the plotplot umbrella. Deltas only; everything absent inherits the umbrella.

| Field | Value |
|---|---|
| Product | tilth |
| Tagline | smart code reading for humans and AI agents. |
| Accent | #4E88A6 Sky — code intelligence (from the umbrella Product Accents table) |
| Faces | `tilth <query>` (CLI) · `tilth --mcp` (MCP, tools `tilth_*`) · `skills/SKILL.md` (the skill) · `tilth-core` (the library) |
| Commands | `tilth <path>` · `tilth <symbol> --scope .` · `tilth <symbol> --callers` · `tilth <file> --deps` · `tilth grok <symbol>` · `tilth diff` |

## Positioning

The read row of the garden. tilth gives agents structural reading of code: token-aware outlines instead of whole files, definition-first search instead of grep, callers and blast radius instead of guesswork, structural diff instead of patch text. It is measured by cost per correct answer, copeca's yardstick. Its parser ships as a library crate, `tilth-core`, that other garden tools link.

## Mark

Four rows of decreasing length read both as an outline's indentation and as tilled soil. One row is sky (`#4E88A6`): the definition you were looking for, with its line-range marker as a filled dot at the left gutter. The other rows and their markers are text-soft (`#786148`) and muted (`#9A8C72`) at half opacity, so the sky row carries the eye. The name is the soil word: tilth is ground worked until it is ready for planting, code read well enough to work in.

File: `assets/logo.svg` (paper). On soil-night the soft rows take the terminal's soft text colour; the sky row does not change.

## Logo usage

- Minimum size: 16px mark height. The page sets it at 26px in the nav and as a 32px favicon on a soil-night tile.
- Clear space: half the mark height on every side.
- Fills are exact: sky `#4E88A6` for the found row and its marker. Never recolour the sky row, never add a fifth row, never outline the wordmark.
- The wordmark is the lowercase word `tilth` in the body face, set in ink beside the mark.
