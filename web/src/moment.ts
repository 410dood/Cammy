// One instant on one camera — the thing every surface here is really trying to
// hand you, and the reason Find can exist without extracting the Events viewer.
//
// `#/live/<cam>/<ts>` opens CameraDetail already scrubbed to that instant, with
// its own timeline to scrub either way. Because that route is the destination,
// a navigator only ever has to produce a (camera, ts) pair — it never has to
// own a player.

export function goToMoment(cameraId: number, ts: number) {
  window.location.hash = `#/live/${cameraId}/${Math.round(ts)}`;
}

export const momentHref = (cameraId: number, ts: number) =>
  `#/live/${cameraId}/${Math.round(ts)}`;
