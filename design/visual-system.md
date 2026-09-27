# VibeStudio visual system

The identity is the lime tile and ink V in [vibestudio-logo.svg](vibestudio-logo.svg).
The supplied [palette](rhythm-palette.json) records the supporting color reference.
This guide owns visual rules and the asset workflow; the SVG and CSS sources own
the exact artwork and active color values. The app uses the existing Tailwind
variables and shared components; no additional component library or styling
framework is needed.

## Logo

- Use the full-color source in both themes. Preserve its geometry, diagonal break,
  aspect ratio and colors; do not recolor it with `currentColor`.
- App chrome uses [VibeStudioMark](../client/web/components/VibeStudioMark.tsx),
  which imports the source SVG directly. Pair the
  decorative image with the visible name **VibeStudio** or an accessible home label.
- Follow the sizing and spacing in [NavBar](../client/web/components/NavBar.tsx)
  and [MobileConnect](../client/web/pages/MobileConnect.tsx). Wordmarks use the
  existing semibold font.
- Native icons use a centered square canvas. Mobile backgrounds are opaque;
  maskable icons keep the V inside the platform's safe area.

## Color roles

App tokens and their light/dark values live in
[globals.css](../client/web/globals.css); public pages share
[brand.css](../docs/brand.css). Use role tokens in components instead of hard-coded
colors. This table maps app tokens to their intended roles rather than duplicating
the CSS values.

| Tokens | Role |
| --- | --- |
| `--brand-lime`, `--brand-ink` | Identity colors |
| `--action`, `--action-fg`, `--action-hover` | Filled primary controls and their paired text/hover colors |
| `--bg` | Workspace canvas |
| `--surface` | Editors, cards, dialogs |
| `--panel` | Grouped controls and sidebars |
| `--fg`, `--muted` | Main and supporting text |
| `--accent`, `--accent-soft` | Links, keyboard focus and selected states |
| `--border` | Dividers and subtle outlines |

`--action` and `--action-fg` are a pair: lime fill with ink text in both themes.
`--accent` is separate because lime text is too pale on paper. Check contrast
against the actual background when changing either token; selected states and
hover backgrounds need checking as well as the page background.

Keep success green, warnings amber, errors red and information teal. Pair status
colors with a label or icon. Preserve language syntax and third-party agent colors
so they retain their meaning. Terminals and editors inherit the shared surface,
text, cursor and selection tokens.

## Controls and layout

- Use `btnPrimary`, `btnGhost`, `btnDanger` and `Badge` from
  [ui.tsx](../client/web/components/ui.tsx), plus the shared
  [Modal](../client/web/components/Modal.tsx). Keep one
  filled primary action per action group; use quiet secondary controls elsewhere.
- Confirm actions through [useConfirm](../client/web/components/useConfirm.ts),
  rather than `window.confirm`, which is not reliably available in the native webview.
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
source together with generated assets. The
[generator](../scripts/generate-brand-assets.mjs) defines output locations and
platform transformations; do not hand-edit the generated copies. Refresh
[dashboard.png](../dashboard.png) and [social-preview.png](../assets/social-preview.png)
from the current interface when its appearance changes.
