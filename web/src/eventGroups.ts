import { CamEvent } from "./api";

// --- A3: smart-detection grouping --------------------------------------------
// Collapse a run of same-camera, same-label detections that happen close in
// time into one "activity" card (best frame + count + duration), the way
// UniFi Protect groups motion into a single smart detection.
export interface Cluster {
  rep: CamEvent; // best (highest-score) frame in the run
  count: number;
  startTs: number; // oldest
  endTs: number; // newest
}
const GROUP_GAP = 120; // seconds between detections that still count as one activity

// Collapse a run of same-camera/same-label detections within GROUP_GAP into one
// representative cluster (highest-score frame + a count). Shared with the camera
// detail rail so a parked car doesn't flood it with identical thumbnails.
export function groupEvents(list: CamEvent[]): Cluster[] {
  const out: Cluster[] = [];
  for (const e of list) {
    // `list` is newest-first, so the cluster's first member is its newest (endTs).
    const last = out[out.length - 1];
    if (last && last.rep.camera_id === e.camera_id && last.rep.label === e.label && last.startTs - e.ts <= GROUP_GAP) {
      last.count++;
      last.startTs = e.ts;
      if (e.score > last.rep.score) last.rep = e;
    } else {
      out.push({ rep: e, count: 1, startTs: e.ts, endTs: e.ts });
    }
  }
  return out;
}
