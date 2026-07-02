# Cammy Design System

Extracted from the live token system in `web/src/styles.css` (the source of
truth, documented in `docs/04-ux-ui-redesign-and-roadmap.md`). UniFi-Protect-
inspired dark NVR language: cool-tinted near-black surfaces, one rationed
electric-blue accent, three-tier elevation, instrument-grade type.

## Theme

Dark by default (`color-scheme: dark`, themed native date/time/select).
Optional `[data-theme="light"]` for daytime wall displays: flips surfaces,
keeps accent and scales. Scene: a homeowner glancing at live video and event
severity, often at night; chrome recedes so video reads.

## Color

12-step cool blue-gray neutral ramp (`--n-0`…`--n-12`, hue ~256, low chroma,
OKLCH behind `@supports` with hex fallback). Never pure black or white.

- Surfaces/elevation: `--bg-shell` (rail/topbar, darkest) < `--bg` (canvas) <
  `--surface` (cards) < `--surface-hover` < `--surface-active`; `--elevated` /
  `--overlay` for popovers; `--scrim` for modals. `--n-0` is the video
  letterbox well.
- Text, 4 rungs: `--text`, `--text-muted`, `--text-subtle`, `--text-faint`.
- Accent (Restrained strategy, ≤10% of surface): UniFi blue `--accent`
  #006fff / oklch(0.595 0.215 256), with `--accent-soft/-tint/-text/-ring`.
  Solid accent means "you can act here".
- Status: `--success` `--warn` `--danger` `--info`, each with a 14–15% soft
  bg (`--*-bg`). Live/REC has its own red `--live`.
- Event-class hues: person=accent, vehicle #7c5cff, audio=warn, face #2dd4bf.

## Typography

Self-hosted Inter Variable (`--font-sans`), no runtime network; mono stack for
data (`--font-mono`). Root 15px. Scale `--text-xs` 0.6875rem → `--text-2xl`
1.875rem. Weights via variable axis: 400 / 510 medium / 590 semibold / 680
bold. `tabular-nums` on data. Eyebrow style: `--fs-eyebrow` + 0.07em tracking.
Line heights: tight 1.2, snug 1.35, body 1.5. Tracking -0.012em on headings.

## Spacing & Radius

4px grid: `--sp-1` 4px … `--sp-10` 64px. Radii: xs 4 / sm 6 / default 10 /
lg 14 / pill 999. Border widths: hairline 1px, strong 1.5px, accent 2px.

## Elevation & Motion

Low-opacity layered shadows (`--shadow-1..3`, `--shadow-pop`) reserved for
overlays; `--edge-light` inset top highlight. Z ladder: sticky 10 / nav 20 /
overlay 40 / modal 60 / popover 80 / toast 100. Motion: 100–360ms
(`--dur-1..4`), ease-out-quint/expo only, transform/opacity only,
`prefers-reduced-motion` honored.

## Components & Recipes

- `web/src/ui.tsx`: Toast, promise-based Confirm/Prompt Dialog, Modal
  (`.modal-wide` variant), `TogglePill` (`<button aria-pressed>`) — the only
  sanctioned toggle.
- `web/src/icons.tsx`: ~60 hand-rolled inline-SVG stroke icons (Lucide-style,
  `currentColor`). No emoji as icons, ever. Sizes via `--ico-*` tokens.
- CSS recipes: `.callout` (+ `-warn`/`-danger`/`-info`), `EmptyState`,
  `.badge`, `details.adv` (progressive disclosure for advanced controls),
  `button.pill`, `.nav-group`, `.evt-legend`, `.cmdk-foot`, `.privacy-tag`,
  `.tune-grid`/`.feat-grid` (aligned form grids), `.tune-foot` (sticky modal
  footer).
- Nav: 3-section rail (Monitor / Detections / Configure); the Monitor group
  equals the mobile bottom tab bar set. Active state = inset box-shadow
  marker (side-stripe borders are banned).
- Patterns: config pages lead with the list, creation forms collapse; filter
  strips show a primary row + "More filters" disclosure; one `:focus-visible`
  ring globally; sticky save bars with dirty-state guards on forms.
