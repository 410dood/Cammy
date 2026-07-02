# Max-effort code review — branch `ux/detection-tuning-modal-redesign` (2026-07-01/02)

Scope: `git diff main...HEAD` (commits `f82ef6a` tuning-modal redesign, `7e5ccbf`
cross-page UX pass, `ad701f4` docs) — 14 files, +1050/−559, web-only + CLAUDE.md.

Process: 10-angle multi-agent finder pass (5 correctness angles + reuse /
simplification / efficiency / altitude / conventions), per-candidate adversarial
verification (CONFIRMED/PLAUSIBLE/REFUTED), then a gap sweep. **51 raw candidates
→ 49 survived verification (all CONFIRMED), consolidating into ~28 unique
defects.** Raw verified list: see the session scratchpad `findings.json`; the 15
most severe were reported via the review UI. This file is the durable handoff.

## Status of the remediation (update as it progresses)

- [x] Review pipeline complete, findings reported (15 ranked, level max)
- [x] Shared groundwork landed by orchestrator: `Callout` primitive in
      `web/src/ui.tsx`; `DAY_NAMES`, `capacityTone`, `fmtDaysLeft` in `web/src/api.ts`
- [ ] 7 parallel fix agents (file-disjoint) — IN FLIGHT, see dispatch map below
- [ ] `cd web && npm run build` (tsc + vite) green
- [ ] Commit + push on `ux/detection-tuning-modal-redesign`
- [ ] Re-report findings with outcomes

## Fix dispatch map (one agent per file group, no overlaps)

| Agent | Files | Findings |
|---|---|---|
| A1 | `pages/Settings.tsx` | hidden-tab invalid input blocks Save (noValidate + switch-tab + reportValidity); h2-string tab map → `data-settings-group` attrs; tablist/tab roles → aria-pressed buttons; passwordless warning hoisted above tabs (visible on default tab); SettingsTabs hook simplification; stale SettingsNav comment; DOW → shared `DAY_NAMES` |
| A2 | `pages/Cameras.tsx` | Add-camera `<details>` un-controlled (one-shot auto-open on empty first load, never force-close); list card before form card in DOM (drop CSS `order`); `scheduleSummary` open-ended/cleared times ("all day" / "from X" / "until X", not "00:00–00:00"); TuneModal settings+capabilities fetch hoisted to page (was per-open); DAYS → `DAY_NAMES`; hand-rolled callouts → `<Callout>`; featCount + feature pills driven from one array |
| A3 | `pages/Alarms.tsx` | first-run auto-open only on first SUCCESSFUL empty load (not on fetch error, not after deleting last rule); Rules card before builder in DOM (drop CSS `order`); scroll+focus builder on "New rule"; local DAY_NAMES → shared |
| A4 | `pages/Events.tsx`, `CameraDetail.tsx`, new `eventGroups.ts` | "More filters" `<details>` never force-closes (onToggle state + force-open-only effect, active-count on summary); groupEvents/Cluster/GROUP_GAP lifted to `web/src/eventGroups.ts`; rail thumbs get `?w=160` |
| A5 | `pages/Home.tsx`, `pages/Recordings.tsx` | **Safari-fatal lookbehind regex** in digest splitter replaced; Recordings severity keys on `days_until_full` only (retention horizon = neutral info; no bare badge when the callout is gated off); Home "~0 days" → `fmtDaysLeft`; shared `capacityTone`; JSX IIFEs → consts; callouts → `<Callout>` |
| A6 | `ZoneEditor.tsx` | `addPoint` guarded `e.isPrimary && e.button === 0`; retry button stops `pointerdown` propagation (click-level guard was dead) |
| A7 | `pages/Signals.tsx`, `pages/FloorPlan.tsx`, `pages/Family.tsx`, `styles.css`, `CLAUDE.md` | Signals idle placeholder → `EmptyState`; FloorPlan upload keyboard-focusable (no `display:none` input); Pets modeStatus honest (object detection works by default → not "Not set up"); dead CSS removed (`.settings-nav*` block, `.fp-drop`; keep `scroll-margin-top`); CLAUDE.md:90 "Not yet committed" corrected to `f82ef6a` |

## The 15 reported findings (ranked most-severe first, all CONFIRMED)

1. **Home.tsx:266** — digest sentence-split regex uses lookbehind `/(?<=\.)\s+/`:
   parse-time SyntaxError on Safari ≤16.3 / iOS <16.4 (Vite target includes
   safari14; esbuild doesn't transpile regex) → the whole bundle fails to parse,
   app white-screens on those browsers (e.g. an older iPad running the Wall view).
2. **Settings.tsx:1088** — SettingsTabs hides cards with `hidden` inside the
   single `<form>`; a constraint-invalid input on a non-active tab (SMTP
   `type=email` "bob@", sample-interval `min=100` given 50) blocks submission
   ("invalid form control is not focusable") with zero feedback — Save silently
   does nothing, dirty state persists.
3. **Events.tsx:639** — React-controlled `open` on "More filters" `<details>`
   force-collapses the panel the instant the last hidden filter empties — while
   the user is typing in the plate input or after "Clear time".
4. **Cameras.tsx:864** — "Add a camera" `<details open={cameras.length === 0}>`
   force-collapses mid-flow when the first camera registers (killing visible scan
   results / half-typed second camera) and force-reopens on last delete.
5. **ZoneEditor.tsx:93** — `addPoint` moved to `onPointerDown` with no
   `e.button`/`e.isPrimary` guard: right/middle-click and secondary touches place
   stray vertices while drawing.
6. **ZoneEditor.tsx:209** — retry button's `e.stopPropagation()` is on *click*,
   but the surface listens on *pointerdown* which already bubbled → pressing
   retry while drawing drops a bogus vertex; the old guard is dead code.
7. **Recordings.tsx:82** — capTone folds `retention_horizon_days` into disk-full
   severity: any retention that prunes at <7 days (e.g. 20 GB cap ≈ 4 days) shows
   a permanent "Filling up"/"Nearly full" badge + warn/danger callout on a
   nearly-empty disk; badge also renders with `write_bytes_per_day === 0` while
   its explanatory callout is gated on write>0 (bare unexplained badge).
8. **Alarms.tsx:116** — first-run auto-open keys on `loaded && rules.length===0`,
   but `loaded` is set in `.finally()` even on fetch error → the "New rule"
   builder opens on top of the ErrorState (also re-opens after deleting the last
   rule).
9. **Settings.tsx:232** — the new "No password set" warn callout lives on the
   security tab, but SettingsTabs defaults to Detection & AI → the warning this
   commit added is never seen in the default flow.
10. **FloorPlan.tsx:125** — empty-state upload is a `<label>`-as-button wrapping
    a `display:none` file input — not keyboard-focusable; the Map page's only
    upload path is unreachable without a mouse.
11. **Settings.tsx:1108** — `role="tablist"/"tab"` without the ARIA tabs pattern
    (no tabpanel/aria-controls, no arrow keys, no roving tabindex).
12. **Cameras.tsx:863 (+ Alarms.tsx:298)** — list/form visually swapped with CSS
    `order` only → DOM order ≠ visual order; keyboard tab order and SR reading
    order inverted (WCAG 2.4.3).
13. **Family.tsx:101** — Pets mode shows "Not set up" whenever no camera has
    `audio_detect`, directly above a live list of dog/cat events (object
    detection is on by default).
14. **Home.tsx:246** — "~0 days until full" rendered when `days_until_full <
    0.5` — the same copy defect this branch fixed on Recordings ("under a day").
15. **Cameras.tsx:36** — `scheduleSummary` shows cleared start/end as
    "00:00–00:00" while the server treats absent times as record-all-day —
    summary implies the opposite of actual behavior.

## Verified cleanup findings below the cap (also being fixed)

- Capacity thresholds (<2 danger / <7 warn) hand-duplicated Home↔Recordings →
  shared `capacityTone` in api.ts (landed).
- Day-name array triplicated (Cameras `DAYS`, Alarms `DAY_NAMES`, Settings `DOW`)
  → shared `DAY_NAMES` in api.ts (landed).
- Hand-assembled `.callout` markup ×~10 across pages (role/aria already drifting)
  → shared `<Callout>` in ui.tsx (landed); pages being converted.
- `SETTINGS_GROUP_OF`: 21 exact `<h2>` strings duplicated from JSX (unmapped
  card silently shows on all tabs) → `data-settings-group` at the source.
- SettingsTabs `activeRef`+useCallback+2 effects → single-effect form; stale
  SettingsNav doc comment above it.
- TuneModal refetches `/api/settings` + `/api/capabilities` on every open →
  hoist to Cameras page.
- `groupEvents`/`Cluster`/`GROUP_GAP` imported from the Events *page* by shared
  CameraDetail → lift to `web/src/eventGroups.ts`.
- CameraDetail rail `<img>`s load full-res snapshots for 84px thumbs → `?w=160`
  (siblings use the cached ThumbQuery resizer).
- Dead CSS: `.settings-nav*` block (~1953–1984) and `.fp-drop` (~1566) have no
  consumers after this branch.
- Signals idle placeholder hand-rolls the EmptyState pattern with inline styles.
- Home: two JSX IIFEs computing plain derivations.
- Alarms/Cameras implement the same "list leads, form collapses" pattern two
  different ways (partially unified by the fixes; full unification deferred).
- CLAUDE.md:90 says the tuning-modal work is "Not yet committed" — it's `f82ef6a`.

## Deliberately deferred (not fixed on this branch)

- **Digest structure**: Home regex-splits backend prose into bullets; the digest
  worker (`crates/core/src/digest.rs`) already has the structured list and joins
  it away — emitting structure is a backend change, out of scope for this
  web-only branch (the Safari-fatal regex is fixed frontend-side).
- Full Alarms↔Cameras collapse-pattern unification into one primitive.

## If resuming after a session break

1. Check agent edits: `git status` / `git diff` in the working tree.
2. Validate: `cd web && npm run build` (tsc + vite must be green).
3. Spot-check in Chrome against the live backend at :8080 (7 real cameras).
4. Commit on `ux/detection-tuning-modal-redesign`, push to origin.
5. Findings JSON (all 49 verified, full failure scenarios):
   `C:\Users\wdood\AppData\Local\Temp\claude\e--dev-ZoomyZoomyCamCam\f4022aec-3ca9-407b-9f75-363de2aaf468\scratchpad\findings.json`
   (temp — this markdown is the durable copy of everything that matters).
