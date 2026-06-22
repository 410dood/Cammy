# docs/05 — Residential Analytics Suite (consumer-camera parity)

Companion to the commercial analytics suite (matrix #53–#61). Where the commercial
track chased VMS/enterprise analytics (line-crossing, occupancy, speed, Re-ID), this
track chases the **consumer / family-safety** market: baby monitors, pet cameras,
pool safety, kid safety, and aging-in-place. Source: a 12-agent competitor-research
workflow (Nanit/Cubo Ai/Miku/Owlet, Furbo/Petcube, SwimEye/Coral/Lynxight/Pool Angel,
Nest/Ring/Arlo/Eufy/Wyze/HomeKit, Nobi/Vayyar/AltumView/Cherry Home, Alexa Guard)
plus a codebase scout and an adversarial completeness/feasibility critic.

## The leverage thesis

We can ship a credible residential-safety suite ("Baby Mode", "Pet Mode", "Pool
Safety", "Aging-in-Place") matching **~70–80% of Cubo Ai / Nanit / Furbo / Petcube /
Nest / Ring** feature lists **with essentially no new ML models**, because the
highest-demand consumer features reduce to **re-scoping primitives we already ship**:

- **YAMNet 521-class audio ontology** ([audio.rs](../crates/core/src/audio.rs)) — already
  classifies *Baby cry/infant cry, Crying/sobbing, Bark, Smoke detector/smoke alarm,
  Glass breaking, Screaming, Gunshot, Doorbell, Siren*. The worker already fires an
  event + AlarmRule + MQTT for **any** AudioSet class name a user adds to
  `settings.audio_labels`. Cry/bark/smoke/glass detection is **already functional** —
  it is a presets-and-UI job, not a backend.
- **PolyZones + directed tripwires + loitering/dwell + occupancy** ([analytics.rs](../crates/core/src/analytics.rs)) —
  the engine behind child-in-zone, pool-perimeter, pet-on-furniture, crib-escape,
  bathroom-overstay, bed-exit.
- **SORT-lite tracker** ([crates/tracker](../crates/tracker)) — persistent IDs, velocity,
  trajectories: per-child / per-pet alerts, pacing/circling, fall-motion reasoning.
- **Face recognition + UNKNOWN/stranger sentinel** (#51) — familiar-face arrival,
  stranger-near-child.
- **CLIP crop Re-ID** (#61) — multi-pet "which pet is which", wildlife coarse-ID.
- **LPR** (#14) — known-vehicle arrival/departure ("teen's car is home").
- **two-way audio** (#43) — doorbell/visitor intercom.
- **anomaly / digest / notification workers** — sleep dashboards, pet diaries,
  "is mom OK today?" wellness checks.

**Two shared "unlock" primitives** dominate the dependency graph:
1. A **child-vs-adult size heuristic** over the existing person detector (no new model
   for v1) → simultaneously enables child-in-zone, child-alone, child-approaching-road,
   pool-unattended-child, stranger-near-child.
2. A **MediaPipe pose-landmarker** (same in-browser family as our hand-gesture
   recognizer) → rollover/prone, crib climb-out, fall, covered-face. **(See the hard
   architectural caveat below — this one is not as cheap as it looks.)**

## Competitive landscape by category

| Category | Incumbents | Their signature AI features |
|---|---|---|
| Baby/infant | Cubo Ai, Nanit, Miku, Owlet, Lollipop, Sense-U | covered-face, rollover/prone, crib climb-out, cry detection, sleep tracking, contactless breathing (sensor-grade) |
| Pet | Furbo, Petcube, Petlibro, Wyze, Eufy, Tapo | dog/cat detect, barking alerts, activity "diary"/time-lapse, multi-pet ID, escape alerts, eat/drink/litter tracking |
| Pool/water | SwimEye, Coral/MYLO, Lynxight, Pool Angel, Sentag, AngelEye | water-entry/perimeter breach, **unattended-child-near-pool**, submerged/motionless (underwater cams) |
| Kid/home | Nest, Ring, Arlo, Eufy, Wyze, HomeKit | person/pet/vehicle/package, activity zones, familiar faces, sound detection, danger zones |
| Elderly | Nobi, Vayyar, AltumView, Cherry Home, Kepler | fall detection, inactivity/long-lie, bed-exit/wandering, overstay, **skeleton-only privacy** |
| Home audio | Nest, Alexa Guard, Ring, SimpliSafe, Abode | smoke/CO chirp, glass-break, dog-bark, baby-cry, speaking |

## Ranked backlog (R01–R24)

Leverage = (market gap × consumer demand) × (how much we reuse vs build new). Effort
S/M/L/XL. Safety-critical features are framed **conservatively** (prevention, not
guarantees) per the liability analysis below.

| ID | Feature | Cat | Reuses | New work | Feas. | Eff. | Lev. |
|---|---|---|---|---|---|---|---|
| **R01a** | **Family & Safety Sounds presets** — sustained (cry/smoke-alarm/bark/siren) | audio | YAMNet engine + `audio_labels` + AlarmRule | preset chips + per-class thresholds | high | **S** | 10 |
| R01b | …transient sounds (gunshot/glass/scream) reliably | audio | YAMNet | **continuous ring-buffer capture** (see gap #1) | med | M | 8 |
| **R02** | Pet detection + off-limits/furniture zone | pet | COCO dog/cat + PolyZone occupancy | "Pet Mode" UI + label-scoped zones | high | **S** | 10 |
| **R03** | Person-enters-pool-zone (perimeter, NOT drowning) | pool | PolyZone + tripwire + person | pool preset + optional inactivity gate | high | **S** | 10 |
| **R04** | Pet-escaped-yard outbound tripwire | pet | directed tripwire + tracker | label-scoped outbound preset | high | **S** | 9 |
| R05 | Pet eat/drink/litter dwell tracking | pet | dwell + occupancy + in/out counts | bowl/litter zone presets + rollup | high | M | 8 |
| R06 | Multi-pet Re-ID (which pet is which) | pet | CLIP crop Re-ID + enroll pattern | pet-identity enroll UI + labeler | **med** | M | 7 |
| R07 | Pet activity diary + time-lapse | pet | digest worker + CrossTimeline + ffmpeg | pet-scoped digest + stitch job | high | M | 8 |
| **R08** | **Child-vs-adult size heuristic** (shared unlock) | child | person detect + tracker bbox + homography | height/scale classifier in `tick` | **med** | M | 9 |
| **R09** | Child-near-pool **UNATTENDED** (no adult in zone) | pool | occupancy split + dwell latch + R08 | child/adult occupancy + edge-trigger | med | M | 9 |
| **R10** | Child-in-restricted-zone (stairs/kitchen/street) | child | PolyZone + tick + R08 | "small-person in zone" rule | high | **S** | 9 |
| R11 | Child-approaching-road tripwire (+ speed escalation) | child | directed tripwire + homography speed + R08 | kid-framed preset | high | S | 8 |
| R12 | Stranger-near-child (unknown face + child present) | child | UNKNOWN_FACE (#51) + R08 occupancy | AND-condition | high | S | 8 |
| **R13** | Familiar-face arrival/departure ("kids home") | child | face rec + door tripwire | match+direction pairing | high | **S** | 8 |
| **R14** | Bathroom/zone overstay (dignity-preserving) | elderly | dwell + occupancy overstay | overstay condition + doorway inference | high | S→M | 9 |
| **R15** | Nighttime wandering / bed-exit (night-gated) | elderly | PolyZone + tripwire + dwell | night predicate + zone-EXIT predicate | high | **S** | 9 |
| R16 | Prolonged inactivity / wellness check | elderly | dwell + occupancy INVERTED + anomaly | absence-of-activity condition | high | M | 8 |
| R17 | Separation-anxiety (bark + pacing + anomaly fusion) | pet | YAMNet + tracker traj + anomaly | **temporal/burst aggregator** (gap #3) | med | M | 7 |
| **R18** | **Pose primitive** (MediaPipe pose-landmarker, shared unlock) | infra | Signals.tsx MediaPipe path + gesture classifier | pose load + posture classifier + binding | **med** | **L→XL** | 7 |
| R19 | Fall detection — assistive (dwell + audio corroboration) | elderly | tracker dwell + YAMNet + R18 | motionless-in-band logic (NOT aspect-flip) | med | L | 6 |
| R20 | Crib climb-out / standing-in-crib | baby | tripwire + PolyZone + R18 | standing pose classifier | med | M | 6 |
| R21 | Rollover / prone (unsafe sleep) | baby | R18 keypoints + crib zone + debounce | supine/prone/side classifier | med | M | 6 |
| R22 | Covered-face / face-blocked (assistive) | baby | run_faces + crib zone + R18 | sustained-absence latch + pose confirm | med | M | 5 |
| R23 | Motion-based sleep dashboard | baby | motion + tracker + YAMNet cry + digests | nightly aggregation + dashboard | high | M | 7 |
| R24 | Motionless-in-water (EXPERIMENTAL, surface-only) | pool | tracker velocity + dwell | near-zero-velocity dwell + consent gate | med | M | 5 |

### Build phases (folding in the critic's reordering)

- **Phase 1 — deterministic quick wins** (no new model, low liability):
  R01a, R02, R03, R04, R10 (adult-agnostic), R13, **+ NEW Doorbell/visitor composite**,
  **+ NEW Known-vehicle arrival/departure**.
- **Phase 2 — shared primitives + child/elderly heuristics**:
  R08 (gated on calibration), **cross-modal confirmation primitive (gap #2)**,
  **temporal/burst aggregator (gap #3)**, R09/R11/R12, R14/R15/R16, R01b ring buffer,
  **+ NEW auto-arm/disarm scheduler**.
- **Phase 3 — pose tier + diaries** (blocked on the server-side pose decision):
  R18 runtime decision → R20/R21/R22/R19, R05/R06/R07, R23, R17, R24.
- **Phase 0 / gating (must precede ANY baby/pool/elderly/fall ship)**: privacy mode
  (skeleton-only / no-clip), **offsite-backup (#70) exclusion for sensitive zones**,
  consent/disclaimer screens, and **miss-mode surfacing** (tell the user when
  monitoring is NOT actually happening).

## Real engineering gaps the critic surfaced (do NOT hand-wave these)

1. **Audio is duty-cycled, so transients can be missed.** The audio worker captures
   `CAPTURE_SECS=1`, loops cameras **sequentially**, then sleeps 2s
   ([audio.rs:30](../crates/core/src/audio.rs#L30), [audio.rs:279](../crates/core/src/audio.rs#L279)).
   Sustained sounds (cry, smoke-alarm cadence, sustained bark) fire reliably; a single
   gunshot or brief glass shatter routinely lands in the dead gap. Transient-safety
   reliability (R01b) needs a **continuous rolling-capture rewrite** — real `audio.rs`
   work, not "free, effort S". Split R01 accordingly.
2. **No cross-modal confirmation primitive.** `AlarmRule::matches` matches a **single**
   label. There is no temporal AND across modalities/events. Glass-vs-dishes,
   scream-vs-TV, fall+thud all want "audio X **and** video Y within N s". Build this
   **early** — R01 precision, R19, R24 all lean on it. (Corroboration may only
   **escalate** a safety alert, never **gate** it.)
3. **No temporal/burst aggregator.** The engine has only a per-(camera,class)
   **cooldown**, which *suppresses* repeats — the opposite of counting them. "Sustained
   barking" (R17), "repeated litter visits" (R05) need a sliding-window rate counter.
4. **Pose is in-browser/foreground only — the big one.** The existing MediaPipe path
   (`Signals.tsx`) runs in **one tab, foreground, one camera, driven by a human opening
   the page**. Safety-critical 24/7 pose monitoring (rollover/fall/climb-out) cannot
   depend on a tab being open. Either build a **headless server-side pose runtime** (new
   server inference — contradicts "no new server model") or accept the pose tier is
   "only while you're watching", which guts its safety value. **R18 is L→XL and the
   whole pose tier (R19-accurate/R20/R21/R22) is BLOCKED on this decision.**
5. **R08 child/adult is fragile and cascades.** Bbox height is meaningless without
   per-camera calibration; ground-plane height (homography `ground_calib`) is robust but
   configured on few cameras. Four safety-critical rules depend on it, so false splits
   become silent safety misses. Ship R08 as **"requires calibration / setup"**, gated.
6. **Fall cheap tier should use dwell, not aspect-flip.** At ~1fps the tall→wide
   transition is mostly unsampled; the dependable signal is **motionless-in-lower-band
   dwell** (R16 machinery), with aspect-flip as a weak hint only.

## Missing categories the critic flagged (high-leverage, were omitted)

- **Doorbell / visitor composite** — we already have YAMNet *Doorbell/Knock* + person +
  entry zone + **two-way audio (#43)** + face rec. A "visitor at door" composite is a
  top-3 residential category that is **almost entirely wired already**. Add to Phase 1.
- **Known-vehicle arrival/departure** — "Dad's car left", "teen's car is home". We ship
  **LPR (#14)** + **Re-ID (#61)** + directed tripwires; AlarmRule plate match exists.
  Pure packaging. Add to Phase 1.
- **Auto-arm/disarm by schedule + presence** — core convenience (Ring/Arlo modes) and a
  false-alarm reducer. We have `arm_mode` + `armed_in_mode`; just need a scheduler/HA
  presence input to flip it. Small worker beside digest/anomaly.
- **Wildlife / nuisance-animal** (raccoon/deer/bird in yard/garden) — COCO has *bird*;
  CLIP coarse-ranks the rest. Growing outdoor niche (Birdfy/Bird Buddy).
- **Weather / false-alarm suppression as a feature** (wind/rain/headlight/shadow
  rejection) — the #1 complaint about cheap cams; framing robust filtering as a feature.

**Under-weighted direct competitors:** Frigate/Frigate+ (the canonical self-hosted NVR;
Frigate+ custom-model marketplace directly answers our package-model gap), Scrypted
(HomeKit Secure Video bridge — the Matter/HomeKit gap), Reolink & Aqara (local-AI
prosumer), Blink, Amcrest (literally in our test rig).

## Out of scope (deliberate)

- **Vision breathing-rate / breathing-cessation** — our 1fps motion-gated sampling is
  far below the dense high-FPS optical flow Nanit/Miku use; severe FDA/medical liability.
- **SpO2 / heart-rate / temperature-humidity** — needs contact/environmental sensor
  hardware we don't have (surface only as MQTT-in from an external HA sensor).
- **True submerged/underwater drowning** — needs underwater cameras; an above-water cam
  cannot see a sunken body (the silent-drowning case). Only prevention (R03/R09) ships.
- **Cry "translation" (hungry/tired/gassy)** — not in YAMNet; needs a new labeled corpus.
- **Fine-grained pet vomit/poop/seizure action recognition** — new action model + data;
  even incumbents are unreliable.
- **Package/porch-pirate** — *package* is not a COCO class our YOLOv8n knows (the one
  notable **model** gap). Surrounding logic exists; needs a package-capable model.
- **AI vet chat / gait-decline** — LLM-services / slow-burn aggregation layers, not the
  CV core.

## Safety / privacy / liability requirements (GATING, not optional)

These are blocking prerequisites for any baby/pool/elderly/fall feature:

- **Pool (R03/R09/R24):** never the word "drowning"; in-product disclaimer + **explicit
  opt-in acknowledgement**; "supplement to fencing/supervision, not a replacement";
  document the above-water blind spot prominently. A false "adult present" (R08 misread)
  = silent miss → ship behind a consent screen.
- **Baby (R21/R22):** these re-open the SIDS-adjacency we avoided by dropping breathing.
  Must carry "NOT a medical device, NOT SIDS/suffocation prevention, does not monitor
  breathing"; require overhead mount + active monitoring.
- **Fall (R19):** false **negatives** are the lethal failure. "May miss falls; not a
  substitute for a medical-alert pendant." Never auto-dial emergency services off one
  unverified visual trigger. Audio corroboration may only **escalate**, never gate.
- **Dignity (R14/R15/R16/R19):** competitors compete on **not showing video**
  (skeleton/radar). Ship a **skeleton-only / no-clip privacy mode** for bedroom/bathroom
  zones, and **exclude these zones from offsite backup (#70)** by default.
- **Minors' biometrics (R12/R13):** children's faces and "stranger near child" clips are
  sensitive. Keep out of offsite backup by default; surface a clear notice.
- **Miss-mode surfacing (all):** when monitoring is NOT happening (camera offline, motion
  gate didn't trip, pose tab closed, audio dead-window) the user must be told. Silent
  non-coverage is the core danger. Extend `health.rs` notifications to cover these.
