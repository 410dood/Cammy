> **Status note (added when this plan was filed, 2026-08-06; updated same day).**
> Phase 1 is SHIPPED — commits `6fb270c`, `e0fca11`, `3ea9825`, `ef0d7a4`,
> `3107f00`, `e803f78`. **Phase 2 is SHIPPED — `a5d2989`** (+ `22d01b2`, a CSV
> export bug found while validating it). Phase 3 is the next task and is NOT
> started. Phases 3-5 are gated: do not collapse the nav until a unified
> Find surface has actually become how footage gets reached.
>
> Phase 2 as built matches this plan with two additions worth knowing:
> `search_corpus` takes a `db::SearchScope` struct rather than four positional
> options, and the empty-scoped-search state offers "Search all time, all
> cameras" in one click (the plan asked for it in its validation steps only).
> A third thing the plan didn't anticipate: **the search now re-runs when the
> window changes**, because leaving results from an abandoned window on screen
> is the same lie in a different direction.
>
> One thing to know before Phase 3: a bare `car` never reaches `/api/search` at
> all — `parseNL` resolves it to a structured `label` filter and the list serves
> it. Only the residual text hits the ranker. Any Find surface that presents
> search as the entry point inherits that split.
>
> Two claims in this plan were overtaken by events and are already done:
> the 200-event wall (fixed in `6fb270c`) and the `#/live/<cam>/<ts>` moment
> route (`e0fca11`). One claim in it is WRONG: it says the Events time inputs
> have "empty aria-labels" — they are properly wrapped in `<label>from</label>`
> / `<label>to</label>`. The real problem was only that they are hidden behind a
> collapsed disclosure.

# THE PLAN — Cammy IA, verified against the tree at `6fb270c`

## 0. What I verified before writing this

Everything below was read, not assumed. The load-bearing facts:

| Claim | Verified |
|---|---|
| `parseHash` does `raw.split("/")` into `[seg, arg]` and **discards the third segment** (`App.tsx:106-114`) | ✅ so `#/live/<cam>/<ts>` is purely additive, zero deep-link risk |
| The 200-event wall is **already gone** — `loadOlder()` at `Events.tsx:802` uses `before: oldest + 1` with id-dedupe, exactly as three proposals proposed to build | ✅ HEAD is `6fb270c feat(web): reach events older than the newest page` |
| Events' only time controls are two bare `<input type="datetime-local">` at `Events.tsx:1154/1158`, no `aria-label`, inside `<details>` labelled *"More filters: hand signal, zone, time range, plate"* (`:1124`) | ✅ |
| `Events.tsx` is **one 1830-line component**; only three tiny pure helpers sit at module scope. The viewer is inline closure-bound JSX. | ✅ — extracting it is a prop-threading refactor, not a move |
| `Recordings.tsx` is the opposite: `ExportRangeCard` (:44), `bucketOf`/`groupLabel` (:170/:175), `MotionSearchModal` (:187), `ScrubGrid` (:339), `SequencePlayer` (:904), `HourRows` (:1053) are all clean module-scope components — **but none are `export`ed** | ✅ cheap to lift |
| Recordings already fetches **1500 events** for the timeline (`:481-488`) and throws them away except as ticks | ✅ per-bucket detection counts are a derivation, not a request |
| `ActivityStrip` bars are non-interactive `<span className="act-col">` inside `role="img"` (`CrossTimeline.tsx:90-104`) | ✅ gating `<button>` on an optional `onPick` leaves Recordings' and CameraDetail's a11y tree byte-identical |
| `CrossTimeline` renders **one `<div className="xtl-evt">` per event per lane**, unbinned (`:248-256`) | ✅ real ceiling — a busy day here is 1026 events (checked `/api/analytics/timeseries`) |
| `/api/analytics/timeseries?days=N` returns `days[{day, ts, count}]` | ✅ live response confirmed — a density calendar needs no new endpoint |
| `SearchQuery` is literally `{ q, limit }` (`api.rs:4668-4672`); `Db::search_corpus(with_embeddings, limit)` (`db.rs:4320`) is `ORDER BY e.ts DESC LIMIT ?1` with **no camera/time/label predicate**; `Events.tsx:902-906` then re-filters the top-N client-side | ✅ a scoped query can honestly return zero while matches exist |
| `list_events` applies `LIMIT` **before** the RBAC `retain` and the tag filter (`api.rs:2471-2480`, self-documented) | ✅ a window-scoped list can under-fill |
| `CameraDetail` already is the unified live↔playback player with `Timeline markTs` + `ActivityStrip`, but is hard-anchored to now (`:162-163` no `before`; `:362` `nowTs={Date.now()/1000}`) | ✅ reachable by camera, never by time |
| Live NVR for validation | ✅ **`:8081`** — v0.4.0, 7 cameras (5 enabled), ~10 130 events. **`:8080` is a different app ("Olari") — do not validate there.** |

---

## 1. Direction: **Three Doors, One Timeline**, staged — with three grafts

I'm taking the winner's destination — **Live · Recap · Find**, where Find is one time-first surface that merges what-happened with what-was-recorded — because the owner's worst pain has exactly one structural cause and every other diagnosis is downstream of it:

> **Events knows WHAT with no time axis. Recordings knows WHEN with content-blind rows. So every "find that clip" starts with a decision the app should never ask — *which page?* — and often needs both.**

Four component passes couldn't fix that because it isn't a component. And the owner described his own job in **pointing** terms ("knows roughly when/where"), which is a direct-manipulation interaction — a day, a lane, a cluster — not a query language.

**But I am staging it the way the winner itself insisted and the implementation-risk panel proved necessary.** The only gates in this repo are `tsc`, `vite build`, and a manual Chrome pass. Big-banging a rewrite of Events (1976 lines) + Recordings (1110) + Home (774) behind those gates is how you lose a month. So Find ships as a **new route beside the living pages**, and nothing is deleted until it has survived a week of real use.

### What I'm rejecting, explicitly

- **"One Clock" as the endpoint.** Its own risk section concedes it: *"'too many places to look' is answered by hiding, not by having fewer real places."* The page-guess decision survives, and its headline deliverable — the `before` cursor — already shipped as `6fb270c`. I'm keeping its three genuinely structural seams (below) and rejecting its conclusion.
- **"One Box" as the front door.** Its backend finding is the best in the set and I'm grafting it. Its *product* bet is wrong for this owner: a fuzzy CLIP ranker presented as *the* answer surface makes a miss read as "Cammy didn't record it" — a trust failure in a security system — and its phone path is a 6-tap modal wizard for a job a day-stepper does in 3.
- **"One Timeline"'s two most expensive moves.** (a) Removing CameraDetail's in-place live↔playback swap to enforce one player — that swap is the single interaction in this app that already feels right; I'm making it the *destination* instead of deleting it. (b) A hand-rolled windowed `FilmStrip` as the default view in phase one — nothing in `web/src` virtualizes anything today, and the proposal concedes an unvirtualized strip with interleaved ffmpeg keyframes is *slower* than the current grid.
- **Any schema change.** None proposed, none needed.
- **Deleting the hour table.** `HourRows` survives behind a toggle. "Show me everything that recorded" is a real question.

### The three grafts

1. **From "One Timeline" — `buildStrip()` + the month density calendar.** The strip is the only idea in the set that actually cures Recordings' content-blindness: ticks on a lane still make you hover to learn anything, whereas an ordered list interleaving event tiles with *coalesced footage spans carrying real keyframe thumbs* lets you **scan quiet time**, which is most of what finding a clip really is. It's a pure function → unit-testable in isolation. The density calendar off `/api/analytics/timeseries` (verified live) makes a fuzzy multi-week memory converge on a visual target instead of failing against a date stepper.
2. **From "One Box" — scoped `/api/search` + the Moment tuple.** The search scoping is the one thing here that is *invisible to any frontend-only plan* and is verified necessary. The Moment tuple (`camera_id`, `ts`) + `/api/recordings/at` is the cheapest abstraction that makes "an event" and "some footage" stop being a routing decision the user has to make.
3. **From "One Clock" — the `#/live/<cam>/<ts>` moment route, the shared `DayStrip`, and the clickable `ActivityStrip`.** These are the cheap structural seams, all verified additive, and — critically — **the moment route is what lets Find ship without extracting the Events viewer.** That single sequencing decision removes the highest-regression-risk act from the entire plan.

---

## Phase 1 — One clock, one moment (biggest relief, zero new pages)

**Why first:** it lands directly on "finding past footage" in the two places he already looks, it is frontend-only, every piece is reused verbatim by Find in Phase 3, and it reverts by `git revert` without touching page structure.

### Add
- **`web/src/DayStrip.tsx`** (~120 lines) — `Today · Yesterday · ◀ [date] ▶ · All` plus a **month density calendar** popover fed by `api.analyticsTimeseries(90)` (module-scope cached for the session — it lands on a hot path). Emits `{from, to} | null`. This is an *extraction*, not an invention: the day→anchor logic already exists at `Recordings.tsx:459-465` (`dayAnchor()`), it just isn't shared.
- **`web/src/moment.ts`** (~50 lines) — `Moment = { key, cameraId, camera, ts, source: "event"|"clip"|"motion"|"similar", event?, segment? }` + `fromEvent` / `fromSegment` / `fromMotionHit`, and `goToMoment(m)` → `#/live/<cam>/<ts>`.

### Change
- **`web/src/App.tsx`** (~15 lines) — `parseHash` reads the third path segment as `focusTs` when `page === "Live"`. Verified additive: today it's discarded. `#/live/<id>` unchanged.
- **`web/src/pages/Live.tsx`** (~3 lines) — thread `focusTs` into `<CameraDetail>`.
- **`web/src/CameraDetail.tsx`** (~45 lines) — new `anchorTs: number | null`, default `null` ⇒ **today's exact behavior, byte-for-byte**. When set: `before: anchorTs + 1` on the two fetches at `:162-163`, passed as `nowTs` to `Timeline` (`:364`) and `ActivityStrip` (`:362`), and `seekTo(focusTs)` on mount — `seekTo` (`:167`) already toasts honestly when no recording covers the instant, so pruned footage is handled. "Back to live" clears it. **This is the only edit that touches a working player: test the null path first and treat any regression there as stop-ship.**
- **`web/src/CrossTimeline.tsx`** (~12 lines) — `ActivityStrip` gains optional `onPick?: (from, to) => void`; bars become `<button>` **only when provided**, and the wrapper's `role="img"` drops to a `role="group"` with a real label in that case. Recordings (`:703`) and CameraDetail (`:359`) pass nothing → unchanged.
- **`web/src/pages/Events.tsx`** — surgical, mostly *deletion*:
  - `<DayStrip>` into the **primary** control row. The two `datetime-local` inputs stay in More filters as the precision fallback and finally get `aria-label`s.
  - `<ActivityStrip onPick={…}>` above `.event-grid` — click an hour, the window narrows.
  - Delete the `all objects` `<select>` (`:1089`) — the Explore pills at `:1183` do the same job *with counts*. Pure duplication.
  - Merge the second `<details>` (Attributes, `:1204`) into the one at `:1118`.
  - Add **"See in timeline"** to the card and the viewer → `goToMoment`.
  - Mirror day/camera/label into the hash (`#/events?day=…&cam=…&label=…`) so Back from a moment restores the list. **Non-negotiable** — without it, leaving the page for a moment loses list state and this phase makes browsing *worse*.
- **`web/src/pages/Recordings.tsx`** — swap the bespoke `day` input (`:619-640`) for `<DayStrip>`; add per-bucket detection counts to `hourGroups` (`:534`) derived from the `events` array already fetched at `:481` — *"11 AM–12 PM · 47 clips · 3 person, 1 vehicle"*. **Never render a bare `0`** for a bucket outside the fetched 1500 — show nothing, or the table starts lying about whether footage is interesting. Add "Open camera here" per lane/row → `goToMoment`.
- **`web/src/styles.css`** — `.daystrip`, `.daycal`, interactive `.act-col` states. Tokens only. Per the documented gotcha, any mobile override goes in a **second** `@media (max-width:768px)` block placed *after* the new base rules.

### Reused, not rebuilt
`Timeline` (already takes `nowTs` + `markTs`), `CrossTimeline`, `ActivityStrip`, `CameraDetail`, `usePolling`/`TogglePill`/`Modal` from `ui.tsx`, `prettyLabel`, `groupEvents`.

### Validate in Chrome against `:8081`
1. `#/events` → DayStrip visible in the **primary** row (not behind a disclosure). Click ◀ three times → grid shows that day only; the URL carries `?day=`; reload restores it.
2. Open the month calendar → days with activity are visibly denser (this NVR has 1026 events on 07/10 and 0 on 07/11-13 — the contrast must be legible).
3. Click a bar in the ActivityStrip → grid narrows to that hour; the strip re-renders scoped.
4. Click **See in timeline** on a `front-door` event → lands on `#/live/3/<ts>`, CameraDetail opens **already scrubbed there**, playhead visible on the Timeline. Click the timeline 20 min earlier → seeks **in place**, no modal.
5. Paste `#/live/3/<ts>` into a fresh tab → same result (cold-load path).
6. Back → Events, same day/camera/label, viewer closed.
7. `#/recordings` → hour rows read *"· 3 person, 1 vehicle"*; scroll to the oldest bucket and confirm it shows **no count**, not `0`.
8. **Regression gate:** `#/live/3` (no ts) → CameraDetail identical to before; ActivityStrip in Recordings and CameraDetail still non-interactive (inspect: `span.act-col`, not `button`).
9. 390 px: DayStrip wraps, ◀/▶ ≥ 40 px, no horizontal body scroll.

**Revert:** one commit, no route removed, no file deleted.

---

## Phase 2 — Scope the search (the only backend change)

**Why it needs the backend, and why it can't wait for Find:** `/api/search` takes only `q`+`limit`; `search_corpus` CLIP-ranks the newest 20 000 events **globally** with no predicate; `Events.tsx:902-906` then time-filters those ~48 survivors client-side. So *"red car on the driveway Tuesday"* ranks the whole database, returns 48 global hits, and the client filters them to **zero while real matches sit in the window**. That is a literal cause of "can't find past footage" and no timeline UI fixes it. The moment Phase 1 makes the day window primary, an unscoped search that silently discards results inside that window becomes an active lie.

### Change (~25 lines, two files, no schema)
- **`crates/core/src/api.rs`** — `SearchQuery` gains `camera_id: Option<i64>`, `after: Option<i64>`, `before: Option<i64>`, `label: Option<String>`; pass through to `search_corpus`. `allowed_cameras` / `camera_allowed` filtering stays **exactly where it is, after** — filters compose *with* RBAC, never around it.
- **`crates/core/src/db.rs`** — `search_corpus` takes the four options and appends the same predicate shape `list_events` already uses at `:2325-2332`: `AND (?n IS NULL OR e.camera_id = ?n) AND (?n IS NULL OR e.ts >= ?n) AND (?n IS NULL OR e.ts < ?n) AND (?n IS NULL OR e.label = ?n)`. It already calls `self.read()`, so it stays correctly on the WAL read pool per the CLAUDE.md rule. **No `SCHEMA_VERSION` bump — no schema touched.**
- **`web/src/api.ts`** — `search(q, limit, opts?)`.
- **`web/src/pages/Events.tsx`** — pass the active DayStrip window + camera into `api.search`; **delete** the client-side re-filter at `:902-906` for search results (it becomes redundant and is the thing that was eating them).

### Cost, stated plainly
This requires the release rebuild + NVR stop/restart dance (the exe is file-locked). Pre-warm `cargo build --release --lib` while it runs, then stop → link → `Start-Process` detached. ~30 s downtime. It cannot be iterated as a pure frontend loop, which is exactly why it is isolated in its own phase.

### Validate on `:8081`
- `curl "localhost:8081/api/search?q=car&camera_id=3&after=<tue00>&before=<wed00>"` → results all `camera_id: 3` and all inside the window. Same query with a window containing **no** cars → empty **and** the UI says *"no matches in this window"*, offering "search all time" as one click.
- Same query unscoped returns a superset. Confirm no result leaks a camera outside `allowed_cameras` using a Viewer-scoped Bearer token over the LAN IP (loopback is admin — the documented gotcha).
- `cargo clippy --all-targets -- -D warnings` + `cargo test` green.

**Revert:** self-contained; all four params are `Option`, so reverting the frontend alone restores today's behavior.

---

## Phase 3 — **Find**, shipped alongside (the merge)

Now the structural move, de-risked by Phases 1-2: Find is a **navigator**, `#/live/<cam>/<ts>` is the **player**, and the Events viewer stays exactly where it is. **This is the sequencing decision that removes the Events-viewer extraction — the single highest-regression-risk act in every proposal — from the plan entirely.**

Find lands at `#/find`, reachable and linkable, **added to the nav without removing anything.**

### 3a — Lift Recordings' parts (pure move, independently verifiable)
`Recordings.tsx`'s sub-components are clean and narrowly-propped but not exported. Move them to `web/src/recordings/`: `ExportRangeCard.tsx`, `MotionSearchModal.tsx`, `ScrubGrid.tsx`, `SequencePlayer.tsx`, `HourRows.tsx`, `buckets.ts` (`bucketOf` + `groupLabel`). `Recordings.tsx` imports them. **Zero behavior change** — validate by diffing the rendered Recordings page against a screenshot taken before the move.

### 3b — The merge
- **`web/src/find/strip.ts`** — the thesis, in one pure function:
  ```ts
  type StripItem =
    | { kind:"event";   ts:number; ev:CamEvent; cluster?:CamEvent[] }
    | { kind:"footage"; from:number; to:number; cameraId:number; segId:number }
    | { kind:"gap";     from:number; to:number }
    | { kind:"marker";  ts:number; label:string }
  export function buildStrip(events, segments, opts): StripItem[]
  ```
  Reuses `coalesce()` from `CrossTimeline.tsx` (export it — currently module-private at `:18`) for footage spans and `groupEvents()` from `eventGroups.ts` for repeat detections. **Unit-tested in isolation** — the only part of this whole plan that can be.
- **`web/src/find/FilmStrip.tsx`** — renders `StripItem[]`. Event tiles reuse the existing `.ev-card` markup; footage spans use `/api/recordings/{segId}/thumb.jpg` (the P2.4 cached keyframe `ScrubGrid` and `Timeline`'s hover bubble already hit); gaps are thin dividers. **`List | Grid` toggle, and Grid is the default for the first week** — the strip is the better idea but the grid is the known-good triage surface, and I'd rather earn the switch than assume it.
- **`web/src/pages/Find.tsx`** (~350 lines, deliberately thin) — three bands: `DayStrip` + clickable `ActivityStrip` → `CrossTimeline` (all cameras) or `Timeline` (one) → `FilmStrip | Grid`. Filter chips are `TogglePill` rows (What / Who / Where), always visible — no disclosure. Result click → `goToMoment` (Phase 1). Event metadata actions link to `#/events/<id>`, which still works.
- **`web/src/CrossTimeline.tsx`** — **bin the ticks.** Above ~400 events per lane, bucket to ≤400 positions and render one node per occupied bucket with a count in the `title`. Verified necessary (`:248` is one div per event per lane, unbinned); this is real work, not a footnote, and it is the most likely way Find feels *slower* than the pages it replaces.
- **`web/src/App.tsx`** — add `Find` to `PAGES`/`LABELS`/`ICONS`, into the **Monitor** group. Nav is temporarily 13. That is fine; it is a week.

### Honesty requirements (non-negotiable, these are trust features)
- Whenever `results.length === limit` **or** the principal is camera-scoped **or** a tag filter is active, render *"showing the newest N of this window — narrow the range"*. `list_events` applies `LIMIT` before the RBAC retain and the tag filter (`api.rs:2471-2480`), so a window-scoped list genuinely can under-fill. Today's page hides behind "newest 200"; the moment the list claims to be *everything in this window*, silence becomes a falsehood.
- Auto-continue paging at most 3 pages, then stop with a message. Never hammer the DB.
- The search band always names its engine: *"ranked by appearance — top 48 of N scanned in this window"* vs *"all 214 in this window"*, with a one-click "browse this window instead".

### Validate on `:8081`
1. `#/find` → today, 5 lanes, coverage + ticks + activity strip. Step back to 07/10 (1026 events) → **measure**: lane render under 400 ms, no jank on hover. If it's slower than `#/recordings` on the same day, binning isn't done.
2. Job-3 dry run, counted out loud: `#/find` → ◀ to the target day → click the `front-door` tick cluster → moment opens scrubbed. **Target: 3 interactions, no typing, no page guess.**
3. Toggle to List → footage spans show real keyframes and read *"quiet · 2:10–4:35"*; gaps are visible **before** you click into one.
4. RBAC honesty: with a Viewer-scoped token, confirm the under-fill line renders rather than a short list looking complete.
5. 390 px: bands stack, the timeline is scrubbable by touch, no horizontal body scroll.
6. **Regression gate:** `#/events` and `#/recordings` behave exactly as after Phase 1.

**Revert:** delete the `Find` nav entry — every old page is untouched and still primary.

---

## Phase 4 — Collapse the nav (only after a week of real use)

**Gate:** if Find has not become how the owner actually reaches footage within a week, **stop here.** Phases 1-3 stand on their own and the collapse should not follow.

- Nav becomes **Live · Recap · Find + ⚙ Setup**, identical on the desktop rail and the mobile tab bar. `MOBILE_PRIMARY` becomes all three, `MoreSheet` disappears from the nav (the component survives as a sheet primitive), `NAV_GROUPS` and the group-label mechanism are deleted.
- **`pages/Home.tsx` → `pages/Recap.tsx`** — leads with a `LiveStrip` of `LiveVideo` tiles (job 1 at 0 clicks), then **one ranked chronological stack bounded by a last-reviewed watermark**: *"Since 11:42 PM — 14 worth a look"*, with "Mark all reviewed". `importanceScore`/`spotlightReason` extract verbatim from `Home.tsx:42/57`. Insights mounts unchanged as a Trends tab. **Nothing in the app currently knows when you last looked, which makes "what did I miss" unanswerable by construction** — this is the one genuinely absent concept in the whole codebase.
- **`pages/Setup.tsx`** — thin shell mounting `Cameras`, `Alarms`, `Family`, `Signals`, `Faces`, `FloorPlan`, `Settings` **as-is**, so none of them need editing. Careful: `Settings`' `groupFromHash()` parses `#/settings/<group>` specifically — alias it, don't reparent it blindly. That's the quiet breakage every proposal waved at.
- Arm bar + a camera-health chip (`5/5` / `1 offline`) hoist into `.rail-tools` / `.topbar`. **Disarm gets a confirm** — a persistent global control that disarms the house on a mis-click is a real hazard.
- `LEGACY` alias map in `parseHash` — **permanent, not transitional**: `#/events/<id>` (push notifications, `Notifications.tsx`, issued share links), `#/events`, `#/recordings`, `#/home`, `#/insights`, `#/people`, `#/map`, `#/settings/<g>`. Also preserve the two silent-failure handoffs: `sessionStorage("cammy-focus-event")` (frame-seeded search) and `sessionStorage("cammy-events-filter")` (Home's type chips, whose source page is moving).

**Known cost, unfixed in this phase:** the watermark is `localStorage`, so desktop and phone will disagree about what he's already reviewed — a daily papercut for someone who uses both equally. Server-side would need a new endpoint (not a schema change: it can ride the existing settings KV). **Flag it as the first follow-up; do not pretend it's solved.**

### Validate on `:8081`
Every legacy hash in a fresh tab. Every Setup group deep link. Recap watermark: mark reviewed, add a soft-triggered event via `POST /api/cameras/3/trigger`, confirm the "since" count moves. 390 px: three tabs at ~120 px each, no overflow sheet.

---

## Phase 5 — Retire Events and Recordings (gated, reversible)

Only if Find has absorbed both in daily use. Delete the nav entries first and leave the routes alive for a further week; delete the files last, against an **explicit written inventory checked off one-by-one**. `Events.tsx` alone carries ~15 features (share links, evidence, signed `.zip`, journey fusion, lifecycle/track, attribute facets, image search, bulk select, tags, notes, feedback, CSV, NL parsing, grouping, plate watchlists); `Recordings.tsx` adds motion search, time-lapse, scrub grid, range export, storage. **Deleting these files without that checklist WILL quietly drop several.** If the inventory can't be closed, don't delete — two extra routes cost nothing.

---

## What I am NOT doing

- **No schema change, no new table, no `SCHEMA_VERSION` bump.** One additive, `Option`-typed filter on an existing endpoint is the entire backend diff.
- **No Events-viewer extraction.** It's inline JSX in an 1830-line component; the moment route makes it unnecessary.
- **No general virtualization pass.** `FilmStrip` windows itself; the 200-card Events DOM stays a 200-card DOM. Virtualization is a separate, later job.
- **Not deleting `HourRows`.** It moves behind a toggle.
- **Not making a text box the front door.** Search is a *refinement* of a window, not the entry.
- **Not removing CameraDetail's live↔playback swap.** It's the destination, not a casualty.
- **Not touching the design system.** Tokens only. Four passes' worth of a11y labels, `prettyLabel` wrapping, error/empty states and touch targets are preserved by **moving code verbatim, never retyping it**.

## What could make this worse

1. **CrossTimeline's unbinned ticks.** Verified: one div per event per lane. Promoting it to Find's primary navigator without binning makes the new page slower than the old ones — the fastest way to lose the owner.
2. **Find's under-fill can lie.** RBAC retain and tag filter run *after* `LIMIT`. If the honesty line is soft, a scoped or tagged window looks complete when it isn't. In a security system that's worse than today's split.
3. **Leaving the page for a moment loses list state** unless Phase 1's hash-mirrored filters ship. If that work gets cut, **cut "See in timeline" with it** — otherwise Phase 1 makes browse-check-three-things worse than the current stacked modal.
4. **Setup behind a gear is a real discoverability regression.** "Not a daily job" isn't "never". The first alarm-rule edit costs one hunt. Ctrl-K and `#/setup/<group>` soften it; it's a deliberate trade made on his stated priorities, against him, not for him.
5. **The watermark is per-browser.** Named above, not hidden.
6. **The sticky player at 360 px.** 16:9 is 203 px, leaving ~400 px for timeline + results. Without a working collapse-to-mini-bar, mobile Find is worse than today's Recordings. Test on a real device, not a resize.
7. **Phase 2 needs a rebuild + restart**, so it can't be validated in a pure frontend loop, and it touches a security-scoped endpoint — a mistake there is a cross-camera leak, not a UI bug.
8. **The strip could read as a second timeline.** If it does, the merge failed: the strip must be the *content* (thumbnails, what), the lanes the *index* (coverage, when), sharing one playhead and one hover. That's a design risk needing a live check at 1366 px and 390 px before Phase 4 commits.

## Order of relief, restated

Phase 1 alone takes job 3 from *guess a page → expand a disclosure → type two unlabelled datetimes → maybe dead-end* to *pick a day → click an hour → click the moment*, on both existing pages, with no new surface to learn. Phase 2 stops the search from discarding results inside the window you asked about. Phase 3 removes the page-guess entirely. Phase 4 is the payoff for "too many places to look" — and it's the only phase that's optional.