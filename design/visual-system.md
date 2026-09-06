# VibeStudio visual system

The identity is the lime tile and ink V in [vibestudio-logo.svg](vibestudio-logo.svg).
The supplied [palette](rhythm-palette.json) is the reference for supporting colors.
The app uses the existing Tailwind variables and shared components; no additional
component library or styling framework is needed.

## Logo

- Use the full-color source in both themes. Preserve its geometry, diagonal break,
  aspect ratio and colors; do not recolor it with `currentColor`.
- App chrome uses `VibeStudioMark`, which imports the source SVG directly. Pair the
  decorative image with the visible name **VibeStudio** or an accessible home label.
- Use a 28px mark in navigation and 36px on the phone connection screen. Keep at
  least 8px between the mark and wordmark. Wordmarks use the existing semibold font.
- Native icons use a centered square canvas. Mobile backgrounds are opaque;
  maskable icons keep the V inside the platform's safe area.

## Color roles

App tokens live in `client/web/globals.css`; public pages share `docs/brand.css`.
Use role tokens in components instead of hard-coded colors.

| Role | Light | Dark | Use |
| --- | --- | --- | --- |
| Identity lime | `#D2F16A` | `#D2F16A` | Logo, primary fills |
| Identity ink | `#152B2F` | `#152B2F` | Logo, text on lime |
| Page | `#FBFBF9` | `#101C1E` | Quiet workspace canvas |
| Surface | `#FFFFFF` | `#152426` | Editors, cards, dialogs |
| Panel | `#F1F4ED` | `#203235` | Grouped controls and sidebars |
| Text | `#152B2F` | `#EFF4EB` | Body and headings |
| Muted text | `#5E706B` | `#A9B6B0` | Supporting information |
| Accent | `#526719` | `#D2F16A` | Links, focus, selected labels |
| Accent soft | `#EFF7D3` | `#2C3B25` | Selected-state backgrounds |
| Border | `#DCE3DC` | `#304448` | Dividers and subtle outlines |

`--action` and `--action-fg` are a pair: lime fill with ink text in both themes.
`--action-hover` deepens lime in light mode and lightens it in dark mode.
`--accent` is separate because lime text is too pale on paper. The light accent is
a darker derivative of the reference moss. It has 6.12:1 contrast on the page and
5.71:1 on the selected background; ink on lime is 11.65:1.

Keep success green, warnings amber, errors red and information teal. Pair status
colors with a label or icon. Preserve language syntax and third-party agent colors
so they retain their meaning. Terminals and editors inherit the shared surface,
text, cursor and selection tokens.

## Controls and layout

- Use `btnPrimary`, `btnGhost`, `btnDanger`, `Badge` and the shared `Modal`. Keep one
  filled primary action per action group; use quiet secondary controls elsewhere.
- Use ink text on lime, theme-aware text on destructive fills, and `--accent` for
  keyboard focus rings. Do not use a pale brand fill as a text or focus color on
  a light surface.
- Retain Inter, the existing type scale, compact navigation, spacing and radii.
  Branding should frame the workspace without competing with documents or terminals.
- Review both themes at desktop and phone widths, including long content, empty
  states, editors, dialogs and selected controls.

## Updating assets

Run `npm run icons:generate` after editing the source SVG. It regenerates desktop,
iOS, Android, browser and home-screen icons, and the public-page logo. Commit the
source together with generated assets. Refresh `dashboard.png` and
`assets/social-preview.png` from the current interface when its appearance changes.
