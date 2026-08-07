# Control-UX priorities — make configuration speak homeowner, 2026-08-07

The owner's words: *"these are not user friendly or intuitive… blue iris uses a
graphical live view and unifi uses something similar — this is the stuff that I
really want to improve upon."*

The reference fix already shipped (`6ba188b`): the per-camera Detection tuning
modal replaced a comma-separated object textbox with grouped chips, raw 0–1
decimals with outcome-labeled sliders ("More alerts ↔ Fewer false alerts"), and
bare size fractions with a **graphical filter drawn on the camera's own live
frame** ("Smaller than this: ignored"). This doc is the app-wide sweep for
everything else of that class, grounded in (a) fresh competitor research on how
UniFi Protect 6 / Blue Iris 6 / Frigate 0.16 / Reolink / Eufy / Ring / Nest /
Synology present controls, and (b) a file-by-file audit of `web/src`.

## What the field teaches (condensed)

1. **Tune on the picture, not in a form.** Zones, masks, tripwires, size
   filters — even bitrate (Protect 6.1 ROI) — are all drawn on the live frame,
   with the sensitivity slider sitting under the preview.
2. **Live feedback while tuning.** Frigate's Motion Tuner (change threshold →
   watch motion boxes update live), Eufy's Motion Test Mode, Synology's live
   threshold bar. The single most-praised tuning affordance anywhere.
3. **Per-object sliders with plain-language captions** (Reolink: separate
   Person/Vehicle/Pet sliders; "High = accepts low-similarity objects").
4. **Few coarse presets instead of numbers** (Ring's 3-position control framed
   as a tradeoff; Eufy's 7 named levels).
5. **Zones carry the config** (Protect Smart Zones bind objects + sensitivity
   to a region; Nest does per-zone notification toggles).
6. **Modes beat schedules** (Ring Home/Away/Disarmed); the schedules users
   actually create are *suppression* windows ("mail carrier 10:00–10:30") and
   first-class Snooze.
7. **Global defaults + visible per-camera override deltas** (Frigate 0.16's
   defaults/override/pending-changes model). We now do this in one modal; it
   should be the app-wide idiom.
8. **Guided forms with an escape hatch** (Frigate 0.16: schema forms over
   YAML, YAML still there).
9. **Tune by example, not by number** (Frigate+ annotate-the-mistake loop;
   ReoNeura "stop tuning, start asking"). Our "Not this" feedback is this
   thesis — it should be the *front door* of tuning, not a Settings card.
10. **The canonical failure mode is ours too:** Blue Iris's comma-separated
    label DSL in a textbox is exactly what we just removed — and still ship in
    five other places.

Anti-lessons: opaque slider semantics (UniFi's "what does Sensitivity mean"
threads), sensitivity-vs-threshold twin numbers nobody can tell apart
(Synology), and minimalism that removes needed control (Nest can't mute a
familiar face).

## Cross-cutting diagnoses (from the code audit)

- **The good patterns already exist in-tree** — `ObjectPicker`,
  `InheritSlider`, `SizeFilterEditor` (Cameras.tsx), the audio-sounds chips and
  sensitivity range (Settings), the ONVIF relay probe + Test button (Alarms),
  "would have matched N events" preview (Alarms), `RetentionHint`,
  `baseUrlWarning`, honest OpenVINO gating. Nearly every finding below is
  "apply a pattern that's three cards away."
- **"Empty = off / 0 = off / blank = all" appears ≥9 times.** A boolean encoded
  in a text field's emptiness is invisible state; each should be a toggle that
  reveals its field.
- **Silent no-op is the dominant failure mode**: free-text names that must
  match magic strings, toggles whose prerequisites aren't checked, URLs with no
  Test button. Several cards diagnose these after the fact
  (`TranscriptionReadiness`, "not downloaded" badges) — good nets under
  problems the inputs invited.
- **Validation is submit-time and coarse** (one top-level error banner after
  the whole form is filled).

---

## P1 — recurring tuning tasks that are hostile or silently broken

**P1.1 The zone↔alarm free-text join** (the worst developer-ism left).
Zones are drawn graphically but auto-named `zone 1` (ZoneEditor:133), and an
alarm rule's "in zone" is a bare substring textbox (Alarms:930) the user must
fill from memory. A typo/rename yields a rule that saves cleanly and **never
fires** — on the highest-stakes flows (pool, crib). Events.tsx:1204 already
renders a real zone `<select>`; Alarms just doesn't use it.
→ Zone picker in Alarms (scoped to the chosen camera, thumbnail of the
polygon); name-prompt with presets (Pool, Driveway, Porch, Crib…) on zone
finish; a rule-health badge ("no zone named X exists — this rule can never
fire"); and the collapse-the-two-pages move: an **"Alert me about this zone"
button inside ZoneEditor** that creates the pre-scoped rule.

**P1.2 Global Settings detection card = the exact fields we just fixed
per-camera.** "objects (comma-separated, empty = all)" (Settings:2279), "min
confidence (0-1)" (:2307), "motion threshold (0-1)" (:2319), "face match
threshold (0-1)" (:2434), "sample interval (ms)" (:2331).
→ Reuse `ObjectPicker` and `InheritSlider` verbatim (no inherit tier here —
these ARE the defaults); face match and sample interval become 3-stop presets
(Loose/Balanced/Strict; Responsive/Balanced/Light-on-CPU).

**P1.3 Discover-first camera add.** The required field is a raw RTSP URL with
embedded credentials (Cameras:1486); scan/ONVIF exist but only pre-fill the
developer form; camera names reject "Front Door" (magic slug rules in a
footnote); `exec:`/`ffmpeg:` prose sits in the default footer.
→ A wizard: auto-scan → camera cards with live thumbnails → credentials once →
auto-resolve main+sub streams → suggested name from the ONVIF device name →
frame preview → done. "Add manually" becomes host/user/pass/path fields with a
vendor path-template dropdown; the URL is assembled, never typed. Friendly
names everywhere, slug generated silently. Advanced sources behind a
disclosure. (Onboarding's scan currently prints found IPs as dead muted text —
carry them into this same wizard.)

**P1.4 Schedules → paint, don't program.** The Modes schedule is a list of
{day pills, time, mode} transition rows the user must mentally simulate
(Settings:2743); alarm arming is day pills + two time inputs + the spec
sentence "to < from spans midnight" (Alarms:1095).
→ A week grid painted with mode colors (transitions derived, not entered),
presets ("Weekday work schedule", "Nights only"), plain-language summary
underneath. Same grid for rule arming. And adopt the field's actual winner:
**suppression windows + a global timed Snooze** ("quiet 10:00–10:30 for the
mail carrier") — long on our own deferred list.

**P1.5 Seconds/cooldown boxes → duration presets.** "quiet period (s)"
(Alarms:1236), "time between repeat events (s)" (Settings:2343), "within (s)"
confirmation window (Alarms:954), loiter seconds + max occupants
(ZoneEditor:468, :481), pre/post-roll (Cameras:718).
→ One duration-picker component: No limit · 30 s · 2 min · 10 min · 1 h ·
Other…, labeled by outcome ("Don't alert me again for…").

**P1.6 Names the app already knows, typed from memory.** "face contains" /
"plate contains" substring boxes (Alarms:861) while People has enrolled
identities and events carry seen plates; camera `group` free-text datalist that
forks "Outdoor"/"outdoor" (Cameras:1073); archive "cameras (comma-separated
remote names)" (Settings:3057) when the token can fetch the list.
→ Pickers seeded from real data (avatar chips for people; recently-seen plates
one tap to add), free text demoted to a labeled escape hatch.

**P1.7 ZoneEditor drawing is place-only.** No dragging a vertex, no moving a
polygon, no inserting a midpoint, no editing after finish; misplace point 2 of
8 and you redraw (ZoneEditor:94-159). Tripwire direction is "A → B only" with
nothing on the canvas labeling A or B (:610).
→ Draggable vertex handles (geometry is already 0..1 SVG), click-edge to
insert, drag-body to move, Rectangle + Whole-frame presets, live rubber-band
and closed-polygon preview, bigger surface + frame refresh. Tripwires get a
direction arrow on the line that flips live, phrased "into the yard / out of
the yard".

**P1.8 The remaining comma-lists, next to the working picker.** Zone `objects`
(ZoneEditor:457), tripwire `objects` (:625), "Package objects" (Cameras:536).
→ Drop `ObjectPicker` in; package objects becomes toggle groups ("Boxes &
parcels / Bags / Envelopes") mapping to COCO labels internally.

**P1.9 Child height as a decimal fraction of frame height** (Cameras:929, the
UI itself says "fragile") gates the whole pool/child feature set.
→ Make it graphical like `SizeFilterEditor`: drag a person-height marker on
the live frame ("about how tall your child appears here"), or click a recent
detection of the child and read the box height.

**P1.10 AI text fields with no feedback loop.** "AI watch — looks like…"
(Alarms:967) and "AI verification" yes/no prompts (:1014) give zero indication
whether they'd ever match; the rule preview explicitly excludes them.
→ A "Test this description" button running the CLIP gate / VLM against the
last 24 h of crops, showing matching thumbnails; example chips ("delivery
van", "person on a ladder"); pre-written verification questions per object
with "write my own" as the escape.

## P2 — setup-time friction and trust

**P2.1 Honest gating, uniformly.** Pose and OpenVINO are gated honestly
(capability check → disabled control + reason). CLIP zone-state ("without the
models this does nothing — no event", ZoneEditor:555), loiter/occupancy
("needs object tracking"), gait ("enroll on the People page"), child rules
("requires calibration on another tab") are not — the toggle enables and
silently no-ops. → Every prerequisite the app can check becomes an inline
"Needs X — Set up" button; child calibration appears inline in the zone card
when a child rule is ticked.

**P2.2 Live tuning feedback (the Frigate Motion Tuner lesson).** We have no
way to see what the motion gate or a threshold change would do except save and
wait. The pipeline already computes motion regions and burns them on
snapshots. → A "test mode" on the tuning modal: live frame with motion cells /
would-be detections overlaid, updating as the sliders move; walk-test like
Eufy ("walk your yard, see what would have fired").

**P2.3 Per-kind alert actions with proof of delivery.** One text box means
URL/topic/email depending on a dropdown (Alarms:1190); webhook + health-push
URLs in Settings have no Test (Settings:3234); ntfy priority leaks protocol
numbers ("1 · min"). → Per-kind fields with on-blur validation, "Send a test
now" beside every channel (the deterrence probe/Test at Alarms:1148 is the
in-tree model), priorities renamed Quiet/Normal/Important/Urgent, and a guided
ntfy flow (generate private topic + QR).

**P2.4 Provider pickers for the three credential forms.** SMTP ("smtps:// vs
smtp:// port 587" prose, no test email, Settings:3110); S3 offsite (six AWS
vocabulary fields, no Test connection, :2949); Ollama/Ask endpoints (URL +
model name typed blind, :2561, :2615). → Provider-first pickers (Gmail/
Outlook/iCloud; B2/Wasabi/R2/S3; This machine/LAN/Cloud) that pre-fill and
relabel, then probe: list the models the endpoint actually reports, PUT a test
object, send a test email — green/red state before saving.

**P2.5 Family modes: wizard, not manual.** Each mode is a 3–4 step prose
procedure across four pages, including retyping "Crib" into two alarm rules
(Family:30-82); status can only say "Partly set up" because the app can't
verify its own instructions. → Guided flow per mode: pick camera → draw the
zone in-card → the app creates toggles/rules/notification prefs itself → Test
button. Status becomes exact because the app authored the config.

**P2.6 Notification tuning speaks tiers, not outcomes.** "normal and up (skip
routine wildlife)" / "high & critical only" with tiers never defined
(Settings:3219). → Outcome copy ("Skip animals and routine motion", "People
and vehicles only") each with a live "≈N alerts/day at this setting" estimate
from recent events.

**P2.7 Onboarding ends before the product starts.** Steps are Welcome /
Security / Cameras (Onboarding:73) — no notification channel, no starter rule;
the user gets a DVR, not a security system. → Fourth step: "How should we
reach you?" (ntfy install + QR, or email) plus one starter rule ("Person at
any camera while Away") created for them.

**P2.8 Hand-signal + gesture strings.** "armed signals (comma-separated,
empty = any)" placeholder `open_palm, victory` (Settings:2494) sits beside a
friendly prettyGesture dropdown for duress. → Toggle chips with prettyGesture
labels, same as the audio-sound chips.

**P2.9 Retire the raw-identifier vocabulary.** Tooltips instruct "make alarm
rules with label 'package_removed'"; `labelText` permanently renders "Child
alone (child_alone)"; Family says "add rules: 'Standing (standing)' in zone
'Crib'". → Drop parenthetical tokens from labels; every "go make a rule with
label X" sentence becomes a button that creates the rule.

## P3 — lower frequency, still worth it

- **Recordings folder** free-text path, the highest-consequence typo on the
  page (Settings:3425) → Browse… (Tauri) + live "✓ writable · 412 GB free ·
  ~18 days at current rate".
- **Model/speech-model file paths** (Settings:2669, :3434, Cameras:580 model
  override) → fold into ModelsCard with Download buttons ("Fast 75 MB / More
  accurate 150 MB"), per-camera override becomes a select of installed models.
- **Retention: four interacting knobs** (segment s / days / GB / re-encode
  after) → one "Keep footage for:" control with the derived disk bar; the
  6-hour-retention surprise (`d061880`) is the proof this matters.
- **Insights is a dead end** — every stat should deep-link into
  `#/find?day=…` or offer the corrective action ("stop alerting on street
  cars" → zone/label change).
- **Recordings' empty-day → Events link drops the day** you just picked;
  Find's hash schema shows how to carry it.
- **FloorPlan pins key on camera *name*** — a rename orphans the pin and the
  hotspots (FloorPlan:164, CameraDetail:402); also click-to-place with no
  dragging, no FOV cones. Key by id; drag-to-move; optional direction cone.
- **`package_zone` is configurable only by curl** while the UI advertises it
  (Cameras:299, :422) — draw it like any other zone or drop the claim.
- **Import footage** wants a server-side absolute path — file picker or real
  upload.
- **Zone type `required`/`ignore`** → "Only watch inside this area" / "Never
  alert inside this area" segmented control.
- **Ground calibration** hard-codes metres, seeds magic 5×5 → unit toggle
  (m/ft) + "click the two ends of something you know the length of".
- **hwaccel select** lists NVENC/QuickSync/VideoToolbox unprobed (silent CPU
  fallback) → probe and disable like the accelerator dropdown; default
  "Automatic (recommended)".
- **MQTT prefix fields** whose own helper says "the default is fine" → behind
  one Advanced disclosure.
- **Webhook body template** → live preview against the latest real event +
  clickable placeholder chips.
- **Reverse-proxy SSO headers** → explicit trust toggle + presets (Authelia /
  Cloudflare Access / oauth2-proxy) + "show me my current request headers".
- **Events' zone filter** (the correct control Alarms should copy) is hidden
  in a disclosure → promote beside camera/object.
- **Audio sensitivity slider** (already the right widget) → label the ends
  ("Hears more ↔ Only loud, clear sounds"), drop the bare 0.35 readout.

## Sequencing recommendation

1. **P1.1 zone↔alarm binding** — converts a silently-broken flagship flow into
   a working one and unblocks P2.5 Family.
2. **P1.2 global Settings detection card** — same components, biggest visual
   payoff per hour, finishes the story `6ba188b` started.
3. **P1.8 + P1.5 + P1.6** — mechanical picker/preset swaps, low risk.
4. **P1.3 camera wizard** — the first thing every new user touches.
5. **P1.7 ZoneEditor editing + P1.9 child height** — the graphical-config
   investments.
6. **P1.4 schedules + P2.x** thereafter.

Everything in P1 is web-only except P1.10's test endpoints (small API
additions) and parts of P1.3 (vendor path templates, name slugify).
