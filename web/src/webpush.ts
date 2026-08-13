// Browser-side Web Push subscription, shared by the Settings notifications
// card and the two wizards (Onboarding, Family) — docs/11 P2: the wizards
// hard-coded ntfy, so a homeowner unwilling to install another app simply got
// no alerts. The built-in channel needs no app at all.
//
// Deliberately its own module (not api.ts): it touches Notification /
// serviceWorker, and api.ts must stay importable from the node-based unit
// tests.

import { api } from "./api";

export const webPushSupported = () =>
  "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;

function urlBase64ToUint8Array(b64: string): Uint8Array {
  const padding = "=".repeat((4 - (b64.length % 4)) % 4);
  const base64 = (b64 + padding).replace(/-/g, "+").replace(/_/g, "/");
  const raw = window.atob(base64);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

/** Ask permission, subscribe this browser, and register the subscription with
 *  the server. Throws with a human-readable message on any refusal. */
export async function enableWebPush(): Promise<void> {
  if (!webPushSupported()) {
    throw new Error("This browser doesn't support push notifications.");
  }
  const perm = await Notification.requestPermission();
  if (perm !== "granted") {
    throw new Error("Notification permission was not granted.");
  }
  const { public_key } = await api.pushVapid();
  const reg = await navigator.serviceWorker.ready;
  const sub = await reg.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: urlBase64ToUint8Array(public_key).buffer as ArrayBuffer,
  });
  await api.pushSubscribe(sub.toJSON());
}

/** Whether this browser already holds a push subscription. */
export async function webPushSubscribed(): Promise<boolean> {
  if (!webPushSupported()) return false;
  try {
    const reg = await navigator.serviceWorker.ready;
    return (await reg.pushManager.getSubscription()) != null;
  } catch {
    return false;
  }
}
