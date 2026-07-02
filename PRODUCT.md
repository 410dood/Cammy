# Product

## Register

product

## Users

Self-hosting home surveillance users: homeowners, families, and prosumers who run
Cammy on a home machine or NAS. They check camera feeds and event history on a
desktop browser or phone, often quickly ("what just happened in the driveway?"),
sometimes at night on a wall display or in a dim room. A secondary persona is the
tinkerer integrating Cammy with Home Assistant / MQTT / scripts. They chose
self-hosting to avoid subscriptions and cloud lock-in; they value control,
privacy, and their data staying local.

## Product Purpose

A self-hosted, cross-platform NVR: Blue Iris-class features without the Windows
lock-in, Frigate-class local AI without Linux/Docker/Coral. Every AI feature is
free and local. Success looks like: the user opens the app and immediately sees
what matters (ranked, curated, severity-tiered activity), configures cameras
once with good defaults, and trusts alerts enough to act on them.

## Brand Personality

Calm, capable, instrument-grade. The UI should feel like a well-built appliance
(UniFi Protect is the explicit balance benchmark): quiet chrome, video-first,
confident defaults, no marketing noise inside the product. Safety-adjacent
features are always framed as assistive aids, never as medical or life-safety
devices.

## Anti-references

- Blue Iris / ZoneMinder-style density: walls of raw toggles, every threshold
  exposed, configuration as the primary surface.
- SaaS dashboard clichés: hero-metric cards, gradient accents, identical card
  grids, upsell banners, cloud-account nags.
- Consumer-cam subscription UX (Ring/Blink): locked features, paywall badges,
  engagement-bait notifications.
- Emoji-as-iconography (removed project-wide; keep it out).

## Design Principles

- **Curation first**: invest in what the user sees on open — ranked feeds,
  grouped events, severity surfaced in color — not more analytics toggles.
- **Opinionated defaults, few knobs**: a good default plus one severity slider
  beats ten exposed thresholds. Advanced knobs fold behind progressive
  disclosure (`details.adv`), never lead.
- **Leanness is a feature**: deliberate omissions are part of the product
  (docs/08 anti-feature list). Do not add surfaces to look complete.
- **Severity escapes grey**: action-required status must escalate out of muted
  text into semantic warn/danger callouts and badges, with a named fix action.
- **Video is the content**: chrome stays dark and recessive; live frames,
  snapshots, and timelines carry the visual weight.
- **Assistive, disclaimed**: residential-safety features carry always-visible
  plain-language caveats, not tooltip-hidden ones.

## Accessibility & Inclusion

Keyboard- and screen-reader-accessible primitives are established (TogglePill
`aria-pressed`, Dialog/Modal/Toast in ui.tsx, one global `:focus-visible`
ring); reuse them, never regress to div-click patterns. Colorblind-safe
timeline/heatmap legends. `prefers-reduced-motion` honored. Touch targets work
on tablets/phones (mobile bottom tab bar is a first-class surface). Dark theme
default with `color-scheme: dark` native widgets; optional light theme for
daytime wall displays.
