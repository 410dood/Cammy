import { useEffect, useRef, useState } from "react";

/** docs/11 P3 — subscribe to the live event feed (`GET /api/events/stream`,
 *  SSE) and invoke `onEvent` for each new event, instead of discovering them
 *  by polling. Returns whether the stream is currently connected, so the
 *  caller can keep its poll as the fallback (slow while connected, fast while
 *  not) rather than trusting the socket blindly.
 *
 *  - RBAC is the server's: the feed is filtered identically to the events
 *    list at connect time (a scoped user only receives their cameras).
 *  - Reconnects are EventSource's own: on error the browser retries with
 *    backoff while we report `connected: false`; a scope change applies on
 *    the next reconnect, same as the list's read-once model.
 *  - Kept out of api.ts so the node unit tests can still import that module
 *    (EventSource and React don't exist there). */
export function useEventStream(onEvent: () => void): boolean {
  const [connected, setConnected] = useState(false);
  const cb = useRef(onEvent);
  cb.current = onEvent;
  useEffect(() => {
    let es: EventSource | null = null;
    try {
      es = new EventSource("/api/events/stream");
    } catch {
      return; // ancient browser — the caller's poll still covers everything
    }
    es.onopen = () => setConnected(true);
    es.onerror = () => setConnected(false);
    es.onmessage = () => cb.current();
    return () => es?.close();
  }, []);
  return connected;
}
