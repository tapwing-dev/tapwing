# Tapwing brand

The mark is a tap point (the dot) followed by three feathers of a wing. It is
monochrome only: black `#111214` on light backgrounds, white on dark ones.

| File | Use |
| --- | --- |
| `tapwing-logo.svg` | Mark + wordmark, `currentColor` (inline SVG in web/docs) |
| `tapwing-logo-black.svg` / `tapwing-logo-white.svg` | Mark + wordmark for `<img>`, README, slides |
| `tapwing-mark.svg` | Mark only, `currentColor` |
| `tapwing-mark-black.svg` / `tapwing-mark-white.svg` | Mark only for `<img>` |
| `favicon.svg` | Favicon; wider gaps for 16 px, switches to white in dark mode |
| `tapwing-avatar.svg` / `tapwing-avatar-512.png` | GitHub org avatar, social/app icon |

The wordmark is Geist Bold (SIL OFL) with -0.04em tracking and the font's own kerning, converted to
outlines so the files need no font.

GitHub README with light/dark switching:

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/brand/tapwing-logo-white.svg">
  <img alt="Tapwing" src="docs/brand/tapwing-logo-black.svg" height="48">
</picture>
```

Keep clear space around the mark of at least the dot's diameter. Don't
recolor, rotate, add effects, or change the angle or spacing of the feathers.
