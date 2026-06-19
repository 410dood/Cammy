# Cammy — UX/UI Redesign & Feature Roadmap

> **Implementation status (2026-06-18):** the full design-system overhaul plus **14
> of the 16 net-new features** in §6 are built and validated (A1–A6, B1–B3, C1–C4,
> C6). Backend: `cargo check`/`clippy -D warnings`/`cargo test` (48) all green; the
> new endpoints/workers were live-validated against real data. Remaining: **C5
> multi-user roles** and **B4 redaction** (depends on C5) — the invasive auth pair,
> intentionally left for a focused, separately-tested pass. See CLAUDE.md.
>
> Status: design strategy, v1 (2026-06-18). This document is the single source of
> truth for the visual redesign and the net-new feature roadmap. It merges four
> synthesis specs (design tokens, iconography, component upgrades, feature backlog)
> with the research corpus (UniFi Protect teardown, competitor UX survey, modern
> dark-system references, AI-era feature scan) and six surface audits
> (shell/nav, live grid, events, camera-detail/timeline, settings/forms,
> cross-cutting a11y/motion). It is written for the real stack: **React 18 + TS +
> Vite, plain CSS custom properties, no Tailwind, no UI library, no router** (pages
> switch via `App.tsx` state), backed by Rust/Axum + SQLite with ONNX/CLIP/whisper
> and a supervised go2rtc child.

---

## Table of contents

1. [Vision & design principles](#1-vision--design-principles)
2. [Design language: the final token system](#2-design-language-the-final-token-system)
3. [Iconography plan + starter SVGs](#3-iconography-plan--starter-svgs)
4. [Component upgrade specs](#4-component-upgrade-specs)
5. [Per-surface redesign notes](#5-per-surface-redesign-notes)
6. [Net-new feature backlog & sequenced roadmap](#6-net-new-feature-backlog--sequenced-roadmap)
7. [Implement first: the one-pass perceived-quality jump](#7-implement-first-the-one-pass-perceived-quality-jump)

---

## 1. Vision & design principles

Cammy is a self-hosted, local-first, privacy-first NVR that should feel
as considered as UniFi Protect while staying true to its "no account, no cloud,
runs on your own box" identity. The redesign closes the gap between an app that
*looks* self-hosted and one that *looks* like a premium instrument: it replaces
emoji-as-icons, native OS widgets, and `window.confirm` with a coherent inline-SVG
icon set, a disciplined dark token system, and hand-rolled components, then re-shapes
the core flows around the way people actually use an NVR - a calm wall to glance at,
a triage inbox to clear, a smart timeline to investigate, and identity-led search to
find a person or plate. The same screen must serve a family member (approachable
defaults, plain-language events) and a power user (keyboard transport, faceted
filters, deep tools one layer down), with zero new heavy dependencies.

The five principles, in priority order:

1. **Local-first, calm by default.** No account chrome, no cloud framing, no
   onboarding that assumes a portal. The default view is quiet: a clean wall, plain-
   language events ("Person at Driveway, 2:14 PM, matched Bill"), and depth disclosed
   progressively. Never expose a raw engineer console as the family-facing surface.

2. **One accent, rationed.** UniFi blue (`#006fff`) is reserved for exactly one
   primary action per view plus selection and focus. Everything else is the cool-
   tinted grayscale ramp. Live/recording red (`--live`) means "the system is hot";
   blue means "you can act here." Color elsewhere is reserved for alarm semantics
   (stranger, panic, plate-of-interest), never decoration. If two things on a screen
   are blue, one is wrong.

3. **Instrument-grade craft.** Stroke icons over emoji, tabular numerals on every
   number that aligns or updates, a 4px spacing grid, tight unified radii, three-tier
   elevation (lightness + hairline + whisper shadow, never a hard drop shadow), and
   subtle 100-240ms ease-out motion. The details are the product.

4. **Triage and investigate, do not list.** Events is a hover-to-preview, mark-as-
   reviewed inbox with a persistent faceted filter bar, not a wall of cards with four
   buttons each. The timeline is the primary investigation control - smart, density-
   aware, keyboard-driven. Lead search with meaning and identity, not metadata.

5. **Accessible and honest about state.** Every control is keyboard-reachable with a
   visible `:focus-visible` ring; icon-only buttons carry `aria-label`s; meaning is
   never color-only. Loading, empty, and error are three distinct, legible states:
   skeletons not blanks, toasts not silent `.catch(() => {})`, themed dialogs not OS
   popups. Respect `prefers-reduced-motion`.

These resolve the few places the source specs disagreed. Where the dark-system spec
favored Inter Variable at a 15px root and the Protect brief leaned on the portable
`Segoe UI` stack at 14px, we adopt **Inter Variable self-hosted with a full system
fallback at a 15px root** - premium when the font loads, never regressing before it
does. Where radius guidance ranged from "tight 6/8/10" (Protect) to "10/14" (dark
system), we unify on the **tighter instrument scale** below (`--radius` 10 default,
`--radius-lg` 14 for tiles/dialogs, `--radius-sm` 6 for controls). Where the icon
spec offered ten distinct hand-pose glyphs, we take the opinionated cut: **one
`IconHand` plus a text label**, building only the three high-value poses
(thumb-up/down/victory) that recur in alarms and events.

---

## 2. Design language: the final token system

Drop-in superset of the current `:root`. Every legacy name the components already
use (`--bg --panel --panel-2 --border --text --muted --accent --accent-soft
--danger --ok --warn --radius`) is preserved as an alias of the new ramp, so nothing
breaks while the new tokens become available. OKLCH is the source of truth, upgraded
behind an `@supports` block so older engines render the hex fallback and capable
engines (Tauri's webview included) get perceptual OKLCH for free, with no per-call-
site change. Paste this in place of the current `:root` block in
`web/src/styles.css`.

```css
/* =====================================================================
   Cammy design tokens — UniFi-blue dark NVR system.
   OKLCH source of truth + hex fallback. Never pure #000/#fff.
   Legacy names kept as aliases so existing components inherit the
   upgrade for free.
   ===================================================================== */
:root {
  color-scheme: dark;            /* dark native date/time/select/scrollbars */

  /* ── NEUTRAL RAMP — 12 steps, cool blue-gray (hue ~256), low chroma.
     Tinted toward UniFi blue, not flat gray. Bottoms at #0a0b0e,
     never #000 (so elevation has headroom). */
  --n-0:  #07080a;  /* video letterbox / deepest well                 */
  --n-1:  #0a0b0e;  /* app shell: rail, topbar (darkest chrome)       */
  --n-2:  #0e1014;  /* page canvas behind content                     */
  --n-3:  #15171c;  /* surface: cards, tiles, sidebar (= legacy panel)*/
  --n-4:  #1c1f26;  /* hovered surface / input fill (= legacy panel-2)*/
  --n-5:  #232730;  /* active / selected surface                      */
  --n-6:  #262a33;  /* hairline border (= legacy border)              */
  --n-7:  #333845;  /* strong border: inputs, focusable edges         */
  --n-8:  #424857;  /* hovered border                                 */
  --n-9:  #565d6e;  /* solid muted / disabled / placeholder           */
  --n-10: #717888;  /* tertiary text                                  */
  --n-11: #9aa3b2;  /* secondary text (lifted from failing #8a919e)   */
  --n-12: #f3f5f8;  /* primary high-contrast text                     */

  /* ── SEMANTIC SURFACES / ELEVATION (3-tier: chrome < content < raised) */
  --bg:            var(--n-2);   /* page canvas                        */
  --bg-shell:      var(--n-1);   /* rail + topbar, darker than content */
  --bg-sunken:     var(--n-1);   /* timeline track, transcript well    */
  --surface:       var(--n-3);   /* cards, tiles, panels               */
  --surface-hover: var(--n-4);
  --surface-active:var(--n-5);
  --elevated:      #21252e;      /* +1 over surface: menus, popovers, toasts */
  --raised:        var(--elevated);   /* alias used by overlay specs   */
  --overlay:       #181a20;      /* dialog body                        */
  --scrim:         rgba(5, 6, 8, 0.82); /* behind modals (+blur)       */

  --panel:   var(--surface);     /* legacy alias                       */
  --panel-2: var(--surface-hover);

  --border:        var(--n-6);   /* default hairline                   */
  --border-strong: var(--n-7);   /* control edges                      */
  --border-hover:  var(--n-8);
  --hairline:      #1a1d24;      /* near-invisible internal divider     */
  --border-soft:   var(--hairline);

  /* ── TEXT — 4-rung hierarchy; --muted lifted to pass AA on --surface */
  --text:        var(--n-12);
  --text-muted:  var(--n-11);    /* secondary, labels, timestamps      */
  --text-subtle: var(--n-10);    /* tertiary                           */
  --text-faint:  var(--n-9);     /* disabled / placeholder             */
  --muted:       var(--text-muted);   /* legacy alias                  */
  --text-on-accent: #f7faff;     /* text on solid blue (not pure #fff) */

  /* ── ACCENT — UniFi blue #006fff. Solid = "you can act": ~1 primary
     + selection + focus. Everything else monochrome. */
  --accent-soft:  rgba(0, 111, 255, 0.14);  /* selected row/tab wash   */
  --accent-tint:  rgba(0, 111, 255, 0.08);  /* faintest hover wash     */
  --accent-border:#2b5fae;
  --accent:       #006fff;                  /* SOLID primary           */
  --accent-hover: #2f86ff;
  --accent-text:  #6ba8ff;                  /* blue text/icon on dark   */
  --accent-ring:  rgba(0, 111, 255, 0.45);  /* focus ring              */

  /* ── STATUS — fg + soft bg. Color rationed to state, not decoration.
     Live/REC is its own red, distinct from accent and from danger. */
  --success: #38c172;  --success-bg: rgba(56, 193, 114, 0.14);
  --warn:    #f5a524;  --warn-bg:    rgba(245, 165, 36, 0.14);
  --danger:  #f0383b;  --danger-bg:  rgba(240, 56, 59, 0.14);
  --info:    #2f86ff;  --info-bg:    rgba(47, 134, 255, 0.14);
  --live:    #ff3b3b;  /* LIVE / REC pip — red = system is hot         */
  --ok: var(--success);                     /* legacy alias            */
  --ok-soft: var(--success-bg);
  --warn-soft: var(--warn-bg);
  --danger-soft: var(--danger-bg);

  /* event-class marker hues (timeline ticks / chips) — muted, distinct */
  --class-person:  var(--accent);
  --class-vehicle: #7c5cff;
  --class-audio:   var(--warn);
  --class-face:    #2dd4bf;

  /* ── TYPOGRAPHY — Inter Variable (1 self-hosted woff2, weight 100–900)
     → system fallback. 510 medium / 590 semibold are Inter's sweet spots. */
  --font-sans: "Inter Variable", "Inter", system-ui, -apple-system,
               "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  --font-mono: "Geist Mono", "SF Mono", "JetBrains Mono", ui-monospace,
               Consolas, monospace;

  /* modular scale, ratio 1.2 (minor third), rem-based, 15px root */
  --text-xs:  0.6875rem; /* 11px  eyebrow / chips / dense meta         */
  --text-sm:  0.8125rem; /* 13px  labels, secondary, table cells       */
  --text-base:0.9375rem; /* 15px  body                                 */
  --text-md:  1.0625rem; /* 17px  section titles / card h2             */
  --text-lg:  1.25rem;   /* 20px  page H1                              */
  --text-xl:  1.5rem;    /* 24px  hero / big counts                    */
  --text-2xl: 1.875rem;  /* 30px  dashboard headline numbers           */
  /* compatibility aliases used by component specs */
  --fs-eyebrow: var(--text-xs);
  --fs-sm: var(--text-sm);
  --ls-eyebrow: 0.07em;

  --fw-normal: 400; --fw-medium: 510; --fw-semibold: 590; --fw-bold: 680;
  --lh-tight: 1.2; --lh-snug: 1.35; --lh-body: 1.5;
  --ls-tight: -0.012em; --ls-normal: 0;

  /* ── SPACE — 4px base grid ──────────────────────────────────────── */
  --sp-1: 4px;  --sp-2: 8px;  --sp-3: 12px; --sp-4: 16px;
  --sp-5: 20px; --sp-6: 24px; --sp-7: 32px; --sp-8: 40px;
  --sp-9: 48px; --sp-10: 64px;

  /* ── RADIUS — tightened, unified, "instrument" not "toy" ─────────── */
  --radius-xs:   4px;   /* chips, tags, small thumbs                   */
  --radius-sm:   6px;   /* buttons, inputs, selects                    */
  --radius:      10px;  /* default cards / tiles (= legacy)            */
  --radius-lg:   14px;  /* camera tiles, dialogs, detail overlay       */
  --radius-pill: 999px; /* status pills, filter chips, toggles         */
  /* short aliases used by component specs */
  --r-xs: var(--radius-xs); --r-sm: var(--radius-sm); --r-md: 8px;
  --r-lg: 12px; --r-pill: var(--radius-pill);

  /* ── BORDER WIDTHS ──────────────────────────────────────────────── */
  --bw-hairline: 1px; --bw-strong: 1.5px; --bw-accent: 2px;

  /* ── ELEVATION — low-opacity, layered, never hard. Pair shadow with
     --edge-light to read "raised" on a dark canvas. Overlays only. */
  --shadow-1: 0 1px 2px rgba(0, 0, 0, 0.35);
  --shadow-2: 0 2px 4px rgba(0, 0, 0, 0.32), 0 4px 12px rgba(0, 0, 0, 0.28);
  --shadow-3: 0 8px 24px rgba(0, 0, 0, 0.45), 0 2px 6px rgba(0, 0, 0, 0.30);
  --shadow-pop: 0 12px 34px rgba(0, 0, 0, 0.55), 0 0 0 1px rgba(0, 0, 0, 0.40);
  --edge-light: inset 0 1px 0 rgba(255, 255, 255, 0.04);

  /* ── Z-INDEX LADDER — documented (fixes mobile-nav-over-overlay bug) */
  --z-base: 0; --z-sticky: 10; --z-nav: 20; --z-overlay: 40;
  --z-modal: 60; --z-popover: 80; --z-toast: 100;

  /* ── MOTION — subtle, fast, ease-out. Animate transform/opacity only. */
  --dur-1: 100ms; --dur-2: 160ms; --dur-3: 240ms; --dur-4: 360ms;
  --ease-out-quint: cubic-bezier(0.22, 1, 0.36, 1);  /* entrances     */
  --ease-out-expo:  cubic-bezier(0.16, 1, 0.30, 1);  /* big overlays  */
  --ease-inout:     cubic-bezier(0.40, 0, 0.20, 1);  /* state swaps   */
  --ease: var(--ease-out-quint);
  /* aliases used by component specs */
  --t: var(--dur-2); --t-slow: 220ms; --ease-out: var(--ease-out-expo);

  font-size: 15px;
  font-family: var(--font-sans);
}

/* Upgrade to OKLCH where supported — single source of truth, no per-site
   changes. Keeps UniFi-blue identity; never pure black/white. */
@supports (color: oklch(0.5 0.1 256)) {
  :root {
    --n-0:  oklch(0.135 0.010 256);
    --n-1:  oklch(0.165 0.012 256);
    --n-2:  oklch(0.190 0.013 256);
    --n-3:  oklch(0.230 0.014 256);
    --n-4:  oklch(0.262 0.015 256);
    --n-5:  oklch(0.295 0.016 256);
    --n-6:  oklch(0.330 0.016 257);
    --n-7:  oklch(0.372 0.017 258);
    --n-8:  oklch(0.438 0.018 258);
    --n-9:  oklch(0.530 0.017 258);
    --n-10: oklch(0.600 0.016 258);
    --n-11: oklch(0.720 0.014 258);
    --n-12: oklch(0.965 0.004 258);
    --elevated: oklch(0.250 0.015 256);
    --overlay:  oklch(0.205 0.013 256);
    --scrim:    oklch(0.135 0.010 256 / 0.82);
    --hairline: oklch(0.215 0.013 256);
    --accent-soft:  oklch(0.595 0.215 256 / 0.16);
    --accent-tint:  oklch(0.595 0.215 256 / 0.08);
    --accent:       oklch(0.595 0.215 256);  /* #006fff */
    --accent-hover: oklch(0.645 0.205 256);
    --accent-text:  oklch(0.760 0.135 256);
    --accent-ring:  oklch(0.66 0.20 256 / 0.85);
    --success: oklch(0.730 0.165 156); --success-bg: oklch(0.730 0.165 156 / 0.15);
    --warn:    oklch(0.800 0.150 78);  --warn-bg:    oklch(0.800 0.150 78 / 0.15);
    --danger:  oklch(0.635 0.215 22);  --danger-bg:  oklch(0.635 0.215 22 / 0.15);
    --info:    oklch(0.645 0.205 256); --info-bg:    oklch(0.645 0.205 256 / 0.15);
    --live:    oklch(0.660 0.235 22);
    --class-vehicle: oklch(0.62 0.20 285);
    --class-face:    oklch(0.78 0.13 185);
  }
}
```

### Required global recipes (paste right after the `:root` block)

These activate the tokens the audits repeatedly demanded - they are not optional;
they fix the WCAG focus failure, the digit jitter, the light native widgets, and
reduced motion in one pass.

```css
/* Tabular numerals on EVERY aligned/updating number: counts, timestamps,
   confidence %, storage, inference ms, timeline ticks. Stops digit jitter. */
.tnum, time, td.num, .metric, .count, .score, .dur, .clock,
.event-card .meta, .tl-bubble {
  font-variant-numeric: tabular-nums;
  font-feature-settings: "tnum" 1;
}

/* One keyboard focus ring everywhere; never strip without replacement.
   Replaces the global `outline:none` on inputs (the WCAG 2.4.7 failure). */
:where(button, a, input, select, textarea, [tabindex],
       .nav-btn, .pill, .chip, .event-card, .feed-item):focus-visible {
  outline: var(--bw-accent) solid var(--accent-ring);
  outline-offset: 2px;
  border-radius: var(--radius-sm);
}
:focus:not(:focus-visible) { outline: none; }

/* Theme native controls so they stop rendering as light OS widgets. */
input[type="text"], input[type="number"], input[type="password"],
input[type="search"], input[type="date"], input[type="time"],
input[type="datetime-local"], select, textarea {
  background: var(--surface-hover);
  border: var(--bw-hairline) solid var(--border-strong);
  border-radius: var(--radius-sm);
  color: var(--text);
}
select {
  appearance: none;
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' fill='none' stroke='%239aa3b2' stroke-width='1.6' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M2.5 4.5 6 8l3.5-3.5'/%3E%3C/svg%3E");
  background-repeat: no-repeat;
  background-position: right 10px center;
  padding-right: 28px;
}
input::-webkit-calendar-picker-indicator { filter: invert(0.7); cursor: pointer; }

@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 0.01ms !important;
    transition-duration: 0.01ms !important;
  }
}
```

### Surface map (which ramp step goes where)

| Step | Token | Where it lives in Cammy |
|-------|-------------------|------------------------------------------------------------|
| n-0 | (video bg) | Camera-tile / lightbox letterboxing only. Never UI. |
| n-1 | `--bg-shell` | Rail + topbar (darkest chrome), timeline track well. |
| n-2 | `--bg` | App canvas behind content. |
| n-3 | `--surface` | Cards, tiles, sidebar body, settings cards. |
| n-4 | `--surface-hover` | Card/nav hover, input fill. |
| n-5 | `--surface-active`| Selected camera, active group tab, secondary buttons. |
| n-6 | `--border` | Default hairlines between cards/rows. |
| n-7 | `--border-strong` | Input borders, focusable control edges. |
| n-9 | `--text-faint` | Disabled text, placeholder icons. |
| n-11 | `--text-muted` | Event meta, camera name, labels, timestamps. |
| n-12 | `--text` | Event titles, page H1, primary readouts. |

**Elevation rule (the discipline that reads premium):** depth in dark = lightness
step + hairline + soft shadow, in that priority. Cards stay flat-with-hairline plus
the whisper `--shadow-1` and `--edge-light`; only true overlays (menus, dialogs,
toasts, the camera-detail panel) use `--shadow-2`/`--shadow-3`/`--shadow-pop`. Never
communicate "raised" with shadow alone on a dark canvas.

**Accent discipline (the single biggest cheap-to-expensive lever):** solid
`--accent` appears at most once per screen - the primary CTA. Selection and focus use
`--accent-soft` + `--accent-border`. Live/recording is `--live` red. Status chips are
monochrome by default and only colorize for alarm-worthy classes.

### Optional light theme (`[data-theme="light"]`)

Because everything is token-driven, a daytime/wall-display theme is a single override
block - flip the semantic tokens, keep the accent and the scales. Toggle with
`document.documentElement.dataset.theme = "light"`, persist in `localStorage`, default
from `prefers-color-scheme` on first load. Keep camera-tile/video backdrops dark
(`--n-0`) regardless of theme; shift only `--accent-text` darker for AA on light.

```css
:root[data-theme="light"] {
  color-scheme: light;
  --bg: #eef0f4;  --bg-shell: #f7f8fa;  --bg-sunken: #e6e9ee;
  --surface: #ffffff;  --surface-hover: #f3f5f8;  --surface-active: #e9edf3;
  --elevated: #ffffff;  --raised: #ffffff;  --overlay: #ffffff;
  --scrim: rgba(20, 24, 33, 0.40);
  --panel: var(--surface); --panel-2: var(--surface-hover);
  --border: #d8dde5;  --border-strong: #c2c9d4;  --border-hover: #aab2c0;
  --hairline: #e6e9ee;  --border-soft: var(--hairline);
  --text: #11151c;  --text-muted: #5a6473;  --text-subtle: #76808f;
  --text-faint: #98a1b0;  --muted: var(--text-muted);
  --accent-text: #0a57c2;
  --accent-soft: rgba(0, 111, 255, 0.12);
  --shadow-1: 0 1px 2px rgba(20, 24, 33, 0.10);
  --shadow-2: 0 2px 6px rgba(20, 24, 33, 0.10), 0 8px 20px rgba(20, 24, 33, 0.06);
  --shadow-3: 0 12px 32px rgba(20, 24, 33, 0.16);
  --edge-light: inset 0 1px 0 rgba(255, 255, 255, 0.6);
}
```

---

## 3. Iconography plan + starter SVGs

Emoji-as-icons is named as the dominant "cheap tell" in all six audits. The system is
a single hand-rolled inline-SVG family in `web/src/icons.tsx` (Lucide-style: 24px
grid, 1.5px stroke, round caps/joins, `currentColor`, zero dependency). The file
already exists, the nav rail and login are already migrated, and roughly 50 content
icons are defined - **the gap is content glyphs**: every page still renders raw emoji
inline. This plan completes the migration.

### State and contract

The shipped `Svg` wrapper already does the right thing: `aria-hidden` by default,
`role="img"` + `<title>` only when a `title` prop is passed, `focusable="false"`,
`currentColor` stroke, `size` prop. Reuse it for new icons; never re-create an
existing one. Filled icons (`IconPlay`, `IconStop`, `IconStar` filled, `IconRecDot`,
`IconStatusDot`) are the deliberate exception, using `fill="currentColor"
stroke="none"` - solid means "active/playing/live."

**Reuse (already defined):** `IconLive, IconBell, IconHand, IconFilm, IconUser,
IconStranger, IconSiren, IconVideo, IconCctv, IconSettings, IconHome, IconSparkles,
IconStar, IconPencil, IconPlay, IconDownload, IconUpload, IconCar, IconMic, IconZone,
IconExpand, IconX, IconSearch, IconCheck, IconSliders, IconCalendar, IconClock,
IconLayers, IconChevron{Up,Down,Left,Right}, IconPlus, IconMinus, IconZoomIn,
IconZoomOut, IconShield, IconLock, IconKey, IconTicket, IconTrash, IconLogIn, IconBan,
IconDatabase, IconWifi, IconWifiOff, IconAlert, IconInfo, IconCommand`.

**Add (8–11 new):** `IconArrow{Up,Down,Left,Right}` (PTZ - full shaft + head, distinct
from chevrons), `IconStop`, `IconRecDot`, `IconStatusDot`, `IconMoon` (snooze),
`IconRefresh` (retry), `IconRadar` (ONVIF scan), and the three high-value hand poses
`IconThumbUp / IconThumbDown / IconVictory`.

### Sizing tokens

```css
:root {
  --ico-nav: 20px;     /* nav rail + page-header icons          */
  --ico-btn: 18px;     /* buttons, toolbar, inline actions      */
  --ico-inline: 16px;  /* in-card meta chips next to text        */
  --ico-dense: 14px;   /* dense meta rows, table cells           */
  --ico-empty: 22px;   /* empty-state / hero glyphs              */
}
.icon, button svg, a svg, .chip svg, .nav-ico svg {
  vertical-align: -0.125em; flex: 0 0 auto;
}
```

Stroke stays 1.5 on the 24-grid at every size (the wrapper keeps the viewBox, so a
smaller render thins the stroke proportionally - correct). At-rest icon color =
`currentColor` inheriting `--muted`; active/hover = `--text` / `--accent`. Reserve
status color for semantics only: `--ok` (known face, allow-listed plate, online,
recording-good), `--warn` (stranger, plate event, connecting), `--danger` (plate-of-
interest, REC dot, failed login, duress), `--accent` (gesture, the one active state).

### Emoji → icon map (the migration contract)

| Surface / context | Emoji today | Icon |
|--------------------------|-----------------------------|---------------------------------------------------|
| Nav rail (done) | 📺🔔✋🎞️👤🚨🎥⚙️ | Live/Bell/Hand/Film/User/Siren/Video/Settings |
| Events search prefix | ✨ | `IconSparkles` (`--muted`) |
| Events filters | 🔔 ⭐ | `IconBell`, `IconStar` (filled) |
| Events identity chips | 👤 🚶 🚗 ✋ ▱ 🎙️ 📝 | User/Stranger/Car/Hand/Zone/Mic/Pencil |
| Events plate state | ⚠ ✓ | `IconAlert` (danger) / `IconCheck` (ok) |
| Events save toggle | ☆ / ★ | `IconStar` (`filled` prop) |
| Events view / export | ▶ ⬇ | `IconPlay`, `IconDownload` |
| Settings audit log | ✅⛔🔑🔓🎫🗑️ | Check/Ban/Key/Lock/Ticket/Trash |
| Settings backup | ⬇ ⬆ | `IconDownload`, `IconUpload` |
| Settings headers | ✋ 🎙️ | `IconHand`, `IconMic` (drop trailing emoji) |
| Alarms conditions | 🚶 ✋ 🎙️ 💤 | Stranger/Hand/Mic/`IconMoon` (new) |
| Signals taxonomy | ✋✊✌️☝️👍👎🤟🤙👌🖐️ | `IconHand` + text label (3 poses only if needed) |
| Signals transport | ▶ ■ 📷 🎥 | `IconPlay`, `IconStop` (new), `IconVideo` |
| Cameras | 📡 🔍 ✓ | `IconRadar` (new), `IconSearch`, `IconCheck` |
| Faces | 👤 | `IconUser` |
| Live PTZ + REC + expand | ▲◀▼▶ ● ⤢ | `IconArrow*` (new), `IconRecDot` (new), `IconExpand`|
| CameraDetail | ✕ 🎙️ 👤 🚗 ▲◀▼▶ | X/Mic/User/Car/`IconArrow*` |
| ZoneEditor / Recordings | ↻ ▶ | `IconRefresh` (new), `IconPlay` |

Render the 10-glyph hand taxonomy as **one `IconHand` + the existing text label**
(`"Open palm"`, `"Fist"`, …) - ten distinct hand poses do not read at 16px and add
maintenance cost. Build `IconThumbUp/Down/Victory` only because they also appear in
alarms and events.

### Starter SVGs (add to `icons.tsx`)

```tsx
/* PTZ directional arrows (full shaft + head, distinct from chevrons) */
export const IconArrowUp    = (p: IconProps) => (<Svg {...p}><path d="M12 19V5"/><path d="m6 11 6-6 6 6"/></Svg>);
export const IconArrowDown  = (p: IconProps) => (<Svg {...p}><path d="M12 5v14"/><path d="m6 13 6 6 6-6"/></Svg>);
export const IconArrowLeft  = (p: IconProps) => (<Svg {...p}><path d="M19 12H5"/><path d="m11 6-6 6 6 6"/></Svg>);
export const IconArrowRight = (p: IconProps) => (<Svg {...p}><path d="M5 12h14"/><path d="m13 6 6 6-6 6"/></Svg>);

/* transport stop + live/status dots (solid; tint via color at call site) */
export const IconStop      = (p: IconProps) => (<Svg {...p} fill="currentColor" stroke="none"><rect x="6" y="6" width="12" height="12" rx="2"/></Svg>);
export const IconRecDot    = (p: IconProps) => (<Svg {...p} fill="currentColor" stroke="none"><circle cx="12" cy="12" r="6"/></Svg>);
export const IconStatusDot = (p: IconProps) => (<Svg {...p} fill="currentColor" stroke="none"><circle cx="12" cy="12" r="5"/></Svg>);

/* snooze, retry, network-scan */
export const IconMoon    = (p: IconProps) => (<Svg {...p}><path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/></Svg>);
export const IconRefresh = (p: IconProps) => (<Svg {...p}><path d="M3 12a9 9 0 0 1 15-6.7L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-15 6.7L3 16"/><path d="M3 21v-5h5"/></Svg>);
export const IconRadar   = (p: IconProps) => (<Svg {...p}><path d="M19.07 4.93A10 10 0 1 0 21 12"/><path d="M12 12 19 5"/><path d="M16.5 7.5a6 6 0 1 0 .9 1.2"/><circle cx="12" cy="12" r="1.2" fill="currentColor" stroke="none"/></Svg>);

/* high-value hand poses (rest reuse IconHand + text) */
export const IconThumbUp   = (p: IconProps) => (<Svg {...p}><path d="M7 10v11"/><path d="M2 11a1 1 0 0 1 1-1h4v11H3a1 1 0 0 1-1-1Z"/><path d="M7 10.5 11 3a2.5 2.5 0 0 1 2.5 3l-.8 3.5H20a2 2 0 0 1 2 2.3l-1.1 6A2 2 0 0 1 18.9 21H7"/></Svg>);
export const IconThumbDown = (p: IconProps) => (<Svg {...p}><path d="M17 14V3"/><path d="M22 13a1 1 0 0 1-1 1h-4V3h4a1 1 0 0 1 1 1Z"/><path d="M17 13.5 13 21a2.5 2.5 0 0 1-2.5-3l.8-3.5H4a2 2 0 0 1-2-2.3l1.1-6A2 2 0 0 1 5.1 3H17"/></Svg>);
export const IconVictory   = (p: IconProps) => (<Svg {...p}><path d="M9 11 7 4.5a1.5 1.5 0 0 1 2.9-.8L11.5 10"/><path d="M15 10.5 16.6 4a1.5 1.5 0 0 1 2.9.7L18 11.5"/><path d="M11.5 10v-.5a1.5 1.5 0 0 1 3 0v1.5l1 .5a3 3 0 0 1 1.5 2.6V17a4 4 0 0 1-4 4h-2a4 4 0 0 1-3.4-1.9L5 15.5a1.6 1.6 0 0 1 2.5-2L9 15V10"/></Svg>);
```

### Accessibility rules at call sites

- **Icon + adjacent visible text (common case):** keep the icon `aria-hidden` (no
  `title`); the SR announces only the text. Never double-read ("person person Bill").
- **Icon-only control:** name the **button** with `aria-label` describing the action
  (`"Pan up"`, `"Fullscreen"`, `"Close"`, `"Save event"`), keep the SVG hidden.
- **State is not color-only:** stranger vs known is `IconStranger` vs `IconUser`;
  plate-of-interest vs known is `IconAlert` vs `IconCheck` - the icon is the non-color
  channel, the text label is the third.
- **Toggles expose state:** save-star and filter chips get `aria-pressed`; convert
  `<span onClick>` filters to real `<button>`s.
- **Decorative duplicates stay silent:** section-header icons and audit-row glyphs are
  `aria-hidden`.

### Migration order

1. Events (densest, most-used). 2. Live + CameraDetail PTZ (also fixes touch/a11y).
3. Settings audit log + cards. 4. Alarms. 5. Cameras / Faces / ZoneEditor /
Recordings. 6. Signals (reuse `IconHand` + text).

---

## 4. Component upgrade specs

All components are hand-rolled plain-CSS + React under `web/src/ui/`, consuming the
tokens above and icons from `icons.tsx`. Zero new dependencies. Two load-bearing
edits to existing CSS: drop `outline:none` on the input `:focus` rule (the global
`:focus-visible` recipe now supplies the ring), and replace the `.nav-btn`
`border-left` active-stripe with a `box-shadow: inset 2px 0 0 var(--accent)` marker
(no layout shift). Reusable recipes:

```css
.card { background: var(--surface); border: 1px solid var(--border);
  border-radius: var(--r-md); padding: var(--sp-5); margin-bottom: var(--sp-4);
  box-shadow: var(--shadow-1), var(--edge-light); }
.card.interactive { cursor: pointer; transition: border-color var(--t), transform var(--t), box-shadow var(--t); }
.card.interactive:hover { border-color: var(--border-strong); transform: translateY(-2px); box-shadow: var(--shadow-2); }
.eyebrow { margin: 0; font-size: var(--fs-eyebrow); text-transform: uppercase;
  letter-spacing: var(--ls-eyebrow); color: var(--muted); font-weight: 600; }
.skeleton { display: block; border-radius: var(--r-sm);
  background: linear-gradient(100deg, var(--panel) 30%, var(--panel-2) 50%, var(--panel) 70%);
  background-size: 200% 100%; animation: sk 1.2s infinite linear; }
@keyframes sk { to { background-position: -200% 0; } }
.empty { border: 1px dashed var(--border); border-radius: var(--r-md);
  padding: 48px 20px; text-align: center; color: var(--muted);
  display: flex; flex-direction: column; align-items: center; gap: var(--sp-3); }
```

### Build priority (highest perceived-quality lift first)

1. **Toast** - kills `alert()`/inline-span feedback; reused by every other
   component's success/error path.
2. **Dialog/Modal** - kills `window.confirm/prompt`, becomes the real media lightbox.
3. **App shell + nav rail** - the persistent frame; fixes the active-stripe, the
   brand/`<h1>` collision, adds the topbar.
4. **Button** - most-instanced control; one disciplined set propagates polish.
5. **Input + Select** - `appearance:none` + custom chevron + `color-scheme:dark`
   removes the most jarring theme break across every filtered view.

Then: Date/Time range, Card/Panel, Pill/Badge/Chip, Skeleton/Empty, Tooltip,
event-card toolbar.

### Specs (condensed)

**Button** - `web/src/ui/Button.tsx`, `<Button variant icon loading disabled>`.
Variants `primary` (the only solid blue per view), `secondary` (`--panel-2` +
border), `ghost` (transparent), `danger` (danger text, danger-soft hover), `icon`
(34×34, muted → raised on hover). `loading` sets `aria-busy`, hides the label
(keeping width so layout never jumps), shows a centered spinner. 44px min hit-area on
touch. Icon-only buttons require `aria-label`. Migrate existing `.primary` →
`btn-primary`, `.ghost` → `btn-secondary`/`btn-ghost`, `.danger` → `btn-danger`; the
inline-styled `<a>` export/clip links become `<a class="btn btn-ghost">`.

**Input + Select** - `web/src/ui/Input.tsx`, `Select.tsx`. `.control` base for
text/number/password/textarea/date/time; field label is its own row above the control
at `#aeb4c0` (brighter than `--muted`, fixing the AA contrast finding). Focus =
accent border + 3px `--accent-soft` glow. Native `<select>` gets `appearance:none` +
SVG chevron for low-traffic cases; high-traffic camera/group filters get a custom
`role="combobox"` + `role="listbox"` with full keyboard support (arrows, Enter/Space,
Esc-restores-focus, Home/End, typeahead). `aria-invalid` → danger border +
`.field-error`.

**Date/Time range** - `web/src/ui/DateTimeRange.tsx`. Replaces the bright native
`datetime-local`/`time` pickers (Events filter, Alarms) with a themed trigger
(`IconCalendar` + tabular-nums span + clear `IconX`) opening a popover: preset chips
(Today / Last 24h / Last 7d / This week / Custom) + month grid(s) + time fields.
In-range cells `--accent-soft`, endpoints solid `--accent`, today a ring. `role="grid"`
with full arrow-key navigation and per-cell `aria-label`. Native `<input type=date>`
fallback on coarse pointers.

**Card / Panel** - three-tier elevation (hairline + `--edge-light` + `--shadow-1`),
standardized `.eyebrow` section header (`<header class="card-head">` with title +
right-aligned actions). `.panel` variant (no padding, `overflow:hidden`) for media
tiles; `.card.interactive` for clickable cards (hover lift to `--shadow-2`).

**Pill / Badge / Chip** - split the three colliding roles. **Badge** (static status,
monochrome by default, colorize only for alarm semantics). **Chip** (interactive
filter toggle, real `<button aria-pressed>`, selected = `--accent-soft` + accent text,
**never green** - green is reserved for liveness/safety). **Status** (dot + label;
online glows ok, offline muted, rec pulses danger). Day-of-week filters become a
`role="group"` of `aria-pressed` chips.

**Toast** - `web/src/ui/Toast.tsx`, `ToastProvider` + `useToast()` →
`toast.success/error/info(msg, opts)`, one `<ToastHost>` portal at app root. Bottom-
right stack, `--raised` surface, variant icon tint only, slide-up enter / fade-down
leave, auto-dismiss 3.5s (errors 6s/sticky), hover pauses the timer, cap visible at
~4 (queue rest). Host `aria-live="polite"` (errors `assertive`). Route all inline
"Saved" spans and swallowed `.catch` errors through it.

**Dialog / Modal** - `web/src/ui/Dialog.tsx`, one primitive for three jobs: confirm
(replaces `window.confirm`), prompt (note/rename inline editor, replaces
`window.prompt`), and **media lightbox** (event snapshot/clip + metadata strip).
`role="dialog" aria-modal="true"`, focus trap (move focus in on open, restore to
trigger on close), **Esc closes every variant**, backdrop click closes, inside-click
`stopPropagation`, always-visible close button. Promise API: `confirm(...)→bool`,
`prompt(...)→string|null`. Camera-detail reuses it as a right-slide panel
(`translateX(100%)→0`).

**Skeleton / Empty** - shimmer placeholders mirroring the real component shape, shown
while the first fetch is in flight; distinguish **first-run** ("Add a camera" CTA),
**no-results** ("No events match these filters - Clear filters"), and **error**
(toast/banner, not a silent empty). Loading and empty must be visually distinct.

**Tooltip** - `web/src/ui/Tooltip.tsx`, `--raised` popover on hover (after ~300ms)
**and focus**, `aria-describedby`, Esc-dismiss. Never the only source of an icon
button's name (that is `aria-label`). Coarse pointers fall back to the visible label.

**Event-card action pattern** - kill the 4 always-on per-card buttons. Keep one
inline primary (save ★ as a corner overlay on the snapshot) and move the rest into a
**hover/`:focus-within`-revealed toolbar** (view recording, clip) plus a `⋯` overflow
menu (note, export, flag). Card is keyboard-activatable (`role="button" tabindex=0` +
Enter/Space → lightbox), not a bare `<div onClick>`. Matches the Protect reveal-on-
hover transport grammar.

---

## 5. Per-surface redesign notes

### Shell & navigation

The persistent frame is a slim icon rail (brand lockup → primary nav → footer
status), a sticky page topbar (the **only** title on screen + right-aligned
contextual actions), and a scrolling content column. Fixes:

- **Active nav** = `--accent-soft` fill + `box-shadow: inset 2px 0 0 var(--accent)`
  marker + accent icon + `--text` label. Remove the `border-left` stripe (it shifts
  text 2px between states).
- **Brand vs `<h1>` collision** - the brand wordmark lives once in the rail; pages
  stop rendering their own masthead `<h1>` and instead pass a `title` (+ optional
  `actions`) up to the shell topbar. The nav already labels the route, so the topbar
  title is the single source of truth.
- **Accessibility** - `<nav aria-label="Primary">`, active button `aria-current="page"`,
  icon `aria-hidden` with the `.nav-label` as the accessible name, brand is a real
  `<button>` returning to Live. Error banner becomes `role="alert"` with a real
  dismiss `<button aria-label="Dismiss">` (`IconX`).
- **Mobile (≤768px)** - rail collapses to a bottom tab bar showing the **5 primary
  tabs** (Live, Events, Recordings, Cameras, Settings) + a "More" sheet; `min-height:
  48px` per tab. Give the bar `z-index: var(--z-nav)` so the camera-detail overlay
  (`--z-overlay`) paints over it (fixes the documented layering bug).

### Live

A clean wall: edge-to-edge video tiles with a bottom gradient scrim carrying name +
status, and transport that fades in on hover (`opacity:.35` at rest for
discoverability, `1` on hover; always-on at touch). Fixes:

- **Icons** - PTZ chevrons → `IconArrow*`, expand ⤢ → `IconExpand`, `● REC` →
  `IconRecDot` in a dark pill with a slow blink on the dot only (reduced-motion off).
- **Tile state** - `LiveVideo` tracks load/error; show a centered "Connecting…" /
  "Stream offline - retry" overlay instead of a silent black tile. Offline tiles
  desaturate (`filter: grayscale(1) brightness(.6)`) with an "Offline" badge. A
  distinct pulsing `--warn` loading dot.
- **Controls** - group tabs become real `<button class="chip" aria-pressed>` (keyboard
  + SR); active chip uses `--accent`, not green. The transport `<select>` joins the
  chip visual system (segmented control or themed trigger), matched in height.
- **Grid** - `minmax(320px, 1fr)` (tiles 2-up earlier), tiles `16/9` to kill
  letterbox bars, 44px PTZ/expand targets on touch.
- **Liveviews** (roadmap A6) - saved named layouts as a tab strip, drag-drop arrange,
  optional follow-motion hero.

### Events

The flagship change: from event log to **triage inbox**. A persistent faceted filter
bar (object · camera · zone · time-range · face/plate, plain-language chips) re-
segments the feed; **Review** mode collapses consecutive motion into one card (best-
frame thumb, class chip, "2:14 PM · 18s", identity/plate) under date headers, hover
auto-plays a low-FPS preview and marks reviewed. Fixes:

- **Cards** - one inline save ★ + hover/`:focus-within` toolbar + `⋯` overflow (kills
  the 4-button clutter). Promote label + time to a header row, demote score to a small
  muted badge, render face/plate/gesture/zone as icon+text chips (meaning survives for
  color-blind users). `.pill.on` → accent, not green.
- **Lightbox** - a real `role="dialog"` with a close button, Esc, focus trap,
  `stopPropagation` on the image, and a metadata strip (label, time, camera, score,
  face/plate/zone badges, caption/transcript/note). Descriptive `alt`.
- **Filter row** - split into left (review/saved toggles), middle (filters), right
  (count + Export); themed `<select>` + custom date-range; `aria-label` each control.
- **Search** - keep the hybrid CLIP + transcript search; add `:focus-within` ring,
  `IconSparkles` (not ✨), and the natural-language parser (roadmap B2) rendering
  editable interpretation chips.
- **States** - skeleton grid while loading, distinct "No results for '{query}'" vs
  "No events match these filters" vs error.

### Camera detail & timeline

A focused-review overlay: big hero player (with a pulsing live dot) + that camera's
mini-timeline + a recent-detections side list, entering as a right-slide panel over a
blurred scrim. The **timeline is the signature instrument** and the priority fix:

- **Timeline** - `role="slider"`, `tabIndex=0`, `aria-label`, `aria-valuemin/max/now`,
  ←/→ seek, `,`/`.` frame-step, `←`/`→` day-step; a visible draggable playhead with a
  tabular-num time bubble; a motion band plus class-colored ticks (person=accent,
  vehicle=violet, audio=warn) instead of all-red; hover-anywhere → thumbnail preview.
  Keep it light: pre-aggregate markers server-side, render the day track to `<canvas>`,
  throttle hover-thumb fetches - explicitly avoid Protect's "slow in Chrome" trap. A
  day-navigator (`‹ Tue Jun 17 ›` + calendar popover) sits above.
- **Feed** - items become real `<button>`s; loading shows skeleton rows (not the empty
  state); failures surface a toast, not a silent `.catch`.
- **Modal** - its own close button, focus trap, and Esc bound to `setPlaying(null)`
  (not the whole overlay).
- **Icons** - mic, person/car badges, close ✕, retry ↻, PTZ → SVG. PTZ/expand to 44px
  on touch.
- **ZoneEditor** - add a "privacy/blur" zone type (roadmap B4); themed `<select>`;
  non-color cue (label text or dashed vs solid stroke) so required/ignore/mask survive
  color-blindness.

### Settings & forms (Settings / Cameras / Alarms / Faces)

The four pages share one form vocabulary, so most fixes fan out from CSS. Priorities:

- **Native controls** - `color-scheme: dark` + themed date/time/textarea + custom
  `<select>` chevron kills three named cheap tells across all four pages at once.
- **Sticky save bar** - wrap the bottom Settings action row in `position: sticky;
  bottom: 0` with a `--panel` background and an "Unsaved changes" pill driven by a
  dirty flag (the longest form has its only Save button 8 cards down).
- **Dialogs** - replace `window.confirm/prompt/alert` (revoke token, restore, delete
  camera, rename/forget face) with the `Dialog` primitive; reuse the existing inline
  `NameCell` commit-on-blur pattern for the Faces rename.
- **Toggles** - style the raw native checkboxes as CSS switches so there is one toggle
  metaphor; convert `<span onClick>` toggles/day-pickers to `<button role="switch"
  aria-checked>` (keyboard + SR).
- **Grouping** - split the 13-control Detection card and 16-control Notifications card
  into sub-groups (Objects & motion / Faces / Plates) with `.eyebrow` dividers.
- **Contrast** - `label.field` lifts off `--muted` to `#aeb4c0` (labels are essential,
  not secondary). Empty/loading states route through `.empty` + skeletons with a CTA.
- **Feedback** - all "Saved ✓" / token-created / resolve-ok inline spans become
  toasts.

---

## 6. Net-new feature backlog & sequenced roadmap

Already shipped (do not re-propose): smart CLIP+transcript search, faces, plates,
gestures, transcription, alarm rules, MQTT/HA discovery, CSV export, backup/restore,
API tokens, audit log, per-camera tuning, zones, PTZ, two-way audio, stranger
detection, Prometheus metrics. Effort: **S** ≤1 day · **M** 2–4 days · **L** ~1 week+.
Impact is 1–5.

### Group A - Signature UniFi-parity wins

| ID | Feature | Effort | Impact |
|----|---------|--------|--------|
| A1 | **Home / Overview dashboard** (new default landing): system health, today's counts by class, last person/vehicle seen, storage-days-left, recent-events strip. New `Home.tsx`, default page; compose from existing endpoints + one optional `/api/overview` aggregator. | M | 5 |
| A2 | **Unified 24/7 cross-camera timeline** (synchronized scrub): all cameras, stacked recording + class-colored event lanes, one playhead drives N seeks. Generalize `Timeline.tsx`; pre-aggregate server-side, `<canvas>` heatmap. | L | 5 |
| A3 | **Smart-detection grouping → Review items**: collapse the flat stream into activity clusters with hover-preview triage. `db::group_events` / `review_items` view, `GET /api/review`, Review-vs-All toggle. | M | 5 |
| A4 | **Notifications Center** (in-app inbox): rail bell + unread badge + slide-in panel deep-linking to events; home for digests + anomalies. New `notifications` table. | M | 4 |
| A5 | **People / Vehicle profile library**: per-identity rollup (sightings, last seen, top cameras, appearance sparkline), tap a face → all footage. `GET /api/identities` aggregation, no new ML. | M/L | 4 |
| A6 | **Liveviews**: saved named camera layouts + drag-arrange + optional follow-motion. `layouts` setting; frontend drag-drop. | M | 3 |

### Group B - AI-era differentiators

| ID | Feature | Effort | Impact |
|----|---------|--------|--------|
| B1 | **AI Daily/Weekly Digest** ("What happened"): auto-generated natural-language recap on Home + optional push. New `digest.rs` cron worker; deterministic templated summary first (no LLM dep), optional Ollama URL for prose polish. | M | 5 |
| B2 | **Conversational / NL search**: parse "red truck in the driveway after dark last week" → time/camera/object/color → existing CLIP+text; render editable interpretation chips. New `nlquery.rs` parser, not new ML. | M | 4 |
| B3 | **Proactive anomaly alerts** ("this is unusual"): per-(camera,label,hour,day-type) frequency histogram → rarity score → `anomaly` mark + AlarmRule condition. New `anomaly.rs`, pure stats on existing data. | M | 4 |
| B4 | **Privacy / redaction UX** (blur, not blackout): privacy-zone blur + role-gated face-blur on snapshots. Extend `ZoneEditor`; CSS/canvas display blur, optional ffmpeg `boxblur` bake-in. Depends on C5. | M | 3 |

### Group C - Polish / quality-of-life

| ID | Feature | Effort | Impact |
|----|---------|--------|--------|
| C1 | **Command palette (Cmd/Ctrl-K)**: fuzzy jump to page/camera/saved search; front door for NL search. New `CommandPalette.tsx`, no deps, no router. | S/M | 4 |
| C2 | **Light theme + toggle**: `[data-theme="light"]` override + rail toggle, remembers + respects `prefers-color-scheme`. | S/M | 3 |
| C3 | **Onboarding / first-run wizard**: set password → ONVIF discover → add camera → recording mode. New `Onboarding.tsx` stepper over existing endpoints. | M | 3 |
| C4 | **Kiosk / Wall mode + PWA install**: chromeless rotating wall + clock + alert ticker; `manifest.webmanifest` + hand-rolled cache-first service worker + Wake-Lock. | M | 3 |
| C5 | **Multi-user roles & accounts**: named users with admin/viewer/live-only; `users` table reusing argon2id + audit plumbing; route→min-role guard. Prerequisite for B4. | L | 3 |
| C6 | **Map / floor-plan camera placement**: upload plan, drop pins with view-cones, click → live, glow on detection. `placements` table + SVG overlay. | L | 2 |

### Cross-cutting UX-elevation track

These recurred in **all six audits** and gate the premium feel of everything above.
They are not features - bundle them into the early sprints: inline-SVG icon set
(emoji removal), Toast + themed Confirm/Prompt dialog, design-token refresh,
`color-scheme:dark` + themed native widgets, global `:focus-visible` ring + real
`<button>` filters/cards, skeletons + `prefers-reduced-motion` + surfaced fetch
errors, event-card action consolidation.

### Sequenced roadmap

**Sprint 1 - perception reset + first AI win (mostly S/M, low risk).**
The cross-cutting UX-elevation track → **A1 Home dashboard** → **B1 AI Daily Digest**.
This flips the "looks self-hosted" verdict almost entirely and stands up the canvas
where the AI story lives.

**Sprint 2 - the differentiation narrative ("ahead of UniFi/Frigate").**
**A3** Review-items grouping → **B3** anomaly alerts → **A4** Notifications Center →
**B2** conversational search → **C1** command palette → **A5** profile library. All
six reuse data/ML already in-tree.

**Sprint 3 - depth + breadth.**
**A2** cross-camera timeline (the perf-sensitive L build) → **C2** light theme, **C3**
onboarding, **C4** kiosk/PWA → **C5** roles (unlocks **B4** privacy blur) → **A6**
Liveviews, **C6** floor-plan map.

**The single highest-leverage thing to build first** is the cross-cutting UX-elevation
track - and within it, the inline-SVG icon set replacing emoji. Every surface audit
names emoji + native widgets + `window.confirm` + missing focus rings as the dominant
cheap tells, every net-new feature inherits the shell, and the icon swap alone is the
highest impact-per-hour change in the whole backlog. If forced to pick one shippable
feature behind it, build **A1 Home + B1 Daily Digest together** - they convert the
shell upgrade into the headline "AI NVR that tells you what happened" story.

---

## 7. Implement first: the one-pass perceived-quality jump

The concrete, ordered list of changes that deliver the biggest perceived-quality jump
in a single pass. Each is near-zero risk, no new dependency, and touches the whole app
at once. Do them in this order - earlier steps unblock later ones.

1. **Design tokens.** Replace the `:root` block in `web/src/styles.css` with the
   superset from §2 (legacy aliases keep everything working), then paste the global
   recipes (tabular-nums, `:focus-visible`, themed native controls, reduced-motion).
   Add the icon sizing tokens. This single edit installs three-tier elevation, the
   cool-tinted ramp, the focus ring, dark native widgets, and digit-stable numbers.

2. **SVG icon set.** Add the ~11 new components from §3 to `web/src/icons.tsx`, then
   migrate content emoji in this order: Events → Live/CameraDetail PTZ →
   Settings audit log → Alarms → Cameras/Faces/ZoneEditor/Recordings → Signals (one
   `IconHand` + text). Apply the call-site a11y rules (`aria-label` on icon-only
   buttons, icon `aria-hidden` next to text, `aria-pressed` on toggles).

3. **Nav / shell.** Build `web/src/ui/AppShell` (rail + sticky topbar). Replace the
   `.nav-btn` `border-left` active-stripe with the inset box-shadow marker; lift the
   page `<h1>` into the topbar (kill the brand/h1 collision); add `aria-label`,
   `aria-current`, the `role="alert"` error banner with a real dismiss button; collapse
   the rail to a 5-tab bottom bar at ≤768px with the corrected z-index ladder.

4. **Events faceted filter + card de-clutter.** Themed filter bar (left toggles /
   middle filters / right count+export) with the custom date-range and `aria-label`ed
   selects; convert group/object filters to real `<button class="chip" aria-pressed>`;
   collapse the 4 per-card buttons to one inline save ★ + hover/`:focus-within`
   toolbar + `⋯` overflow; make the card keyboard-activatable.

5. **Controls - Button + Input/Select.** Ship `web/src/ui/Button.tsx` and
   `Input.tsx`/`Select.tsx`; migrate `.primary`/`.ghost`/`.danger` and the inline-styled
   export/clip `<a>`s to the button classes; apply the custom `<select>` chevron and
   `.control` focus glow everywhere. Enforce accent discipline (one primary per view).

6. **Toasts + Dialogs.** Ship `web/src/ui/Toast.tsx` (provider + `<ToastHost>` at app
   root) and `Dialog.tsx`; grep-replace all `window.alert/confirm/prompt` (~15 sites)
   with `confirm()`/`prompt()`/`toast.*`; route every inline "Saved ✓" span and
   swallowed `.catch(() => {})` through a toast; rebuild the Events snapshot lightbox
   and the camera-detail modal on the `Dialog` primitive (close button, Esc, focus
   trap, metadata strip).

After this pass: emoji, native OS widgets, `window.confirm`, the missing focus ring,
the flat 2-shade elevation, the brand/`<h1>` collision, and the 4-button card clutter
are all gone - the six audits' dominant "cheap tells" resolved in one sweep, leaving
the Home dashboard and AI digest (Sprint 1, §6) to land on a shell that already looks
as premium as they are.

---

*Files this strategy drives:* `web/src/styles.css` (tokens + recipes + light theme),
`web/src/icons.tsx` (new glyphs), `web/src/ui/*` (new components), `web/src/App.tsx`
(shell, nav, default page), and the per-surface pages under `web/src/pages/` plus
`web/src/LiveVideo.tsx`, `web/src/CameraDetail.tsx`, `web/src/Timeline.tsx`,
`web/src/ZoneEditor.tsx`.
