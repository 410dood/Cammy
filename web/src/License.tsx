// Licensing surfaces: a restrained trial/upgrade banner for the app shell, and a
// full status + activation pane for Settings. Both read /api/license.
//
// Deliberately quiet, per PRODUCT.md's anti-references (no upsell banners, no
// paywall badges, no cloud-account nags): the banner stays hidden while licensed
// AND for most of the trial — it only speaks up in the last week, or once the
// trial has elapsed. Nothing here gates features; an expired trial keeps
// recording. It is a nudge to pay for something already earning its keep.
import { useEffect, useState } from "react";

import { api, type Entitlement, type LicenseInfo } from "./api";
import { IconInfo, IconAlert, IconCheck } from "./icons";
import { Callout, ErrorState, useToast, useDialog } from "./ui";

/** Days-left threshold below which the shell banner appears. */
const NUDGE_WINDOW_DAYS = 7;

/** Dismissal is persisted per-day: during the last-week nudge window the banner
 *  comes back at most once a day, not on every page load. */
const DISMISS_KEY = "cammy-license-dismissed";
function dismissedToday(): boolean {
  try {
    return localStorage.getItem(DISMISS_KEY) === new Date().toDateString();
  } catch {
    return false;
  }
}
function rememberDismissal() {
  try {
    localStorage.setItem(DISMISS_KEY, new Date().toDateString());
  } catch {
    // private mode etc — fall back to session-only dismissal
  }
}

function fmtDate(unixSecs: number): string {
  return new Date(unixSecs * 1000).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/** Slim shell banner. Renders nothing unless the trial is ending soon or over. */
export function LicenseBanner() {
  const [info, setInfo] = useState<LicenseInfo | null>(null);
  const [dismissed, setDismissed] = useState(dismissedToday);

  useEffect(() => {
    api.license().then(setInfo).catch(() => {});
  }, []);

  if (!info || dismissed) return null;
  const ent = info.entitlement;

  // Licensed, or comfortably inside the trial → stay silent.
  if (ent.state === "licensed") return null;
  if (ent.state === "trial" && ent.days_left > NUDGE_WINDOW_DAYS) return null;

  const expired = ent.state === "expired";
  return (
    <Callout
      tone={expired ? "warn" : "info"}
      className="license-banner"
      icon={expired ? <IconAlert size={16} /> : <IconInfo size={16} />}
    >
      <div className="license-banner-row">
        <span>
          {expired ? (
            <>
              Your {info.trial_days}-day trial has ended. Cammy keeps running — if it’s
              earning its keep, a one-time license supports it.
            </>
          ) : (
            <>
              {ent.state === "trial" && (
                <>
                  <b>
                    {ent.days_left} day{ent.days_left === 1 ? "" : "s"}
                  </b>{" "}
                  left in your Cammy trial.
                </>
              )}
            </>
          )}
        </span>
        <span className="license-banner-actions">
          <a className="btn btn-primary" href={info.buy_url} target="_blank" rel="noreferrer">
            Buy a license
          </a>
          <a className="btn btn-ghost" href="#/settings/license">
            Already purchased? Activate
          </a>
          {!expired && (
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => {
                rememberDismissal();
                setDismissed(true);
              }}
            >
              Dismiss
            </button>
          )}
        </span>
      </div>
    </Callout>
  );
}

function StatusLine({ ent, trialDays }: { ent: Entitlement; trialDays: number }) {
  if (ent.state === "licensed") {
    return (
      <p className="muted" style={{ marginTop: 0 }}>
        <span className="role-pill role-admin" style={{ marginRight: 6 }}>
          Licensed
        </span>
        {ent.plan === "lifetime" ? "Lifetime license" : "License with updates"} · {ent.email} ·{" "}
        {ent.seats} seat{ent.seats === 1 ? "" : "s"}
        {ent.expires != null && <> · updates until {fmtDate(ent.expires)}</>}
      </p>
    );
  }
  if (ent.state === "trial") {
    return (
      <p className="muted" style={{ marginTop: 0 }}>
        Trial · <b>{ent.days_left}</b> of {trialDays} days remaining (ends {fmtDate(ent.ends)}).
      </p>
    );
  }
  return (
    <p className="muted" style={{ marginTop: 0 }}>
      Trial ended. Cammy is unlicensed but fully operational.
    </p>
  );
}

/** Full license pane for the Settings page: status + key activation/removal.
 *  Activation/removal are Admin-only server-side; a non-admin's attempt 403s. */
export function LicensePane() {
  const toast = useToast();
  const dialog = useDialog();
  const [info, setInfo] = useState<LicenseInfo | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);

  const load = () =>
    api
      .license()
      .then((i) => {
        setInfo(i);
        setLoadErr(null);
      })
      .catch((e) => setLoadErr(String(e instanceof Error ? e.message : e)));
  useEffect(() => {
    load();
  }, []);

  // This card is the whole License tab — it must never render to nothing.
  if (!info) {
    return (
      <div className="card" data-settings-group="license">
        <h2>License</h2>
        {loadErr ? (
          <ErrorState what="license status" message={loadErr} onRetry={load} />
        ) : (
          <p className="muted">Loading license status…</p>
        )}
      </div>
    );
  }
  const licensed = info.entitlement.state === "licensed";

  const activate = async () => {
    if (!key.trim()) return;
    setBusy(true);
    try {
      await api.activateLicense(key.trim());
      setKey("");
      await load();
      toast.success("License activated — thank you.");
    } catch (e) {
      toast.error(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    const ok = await dialog.confirm({
      title: "Remove license from this machine?",
      body: "This computer returns to unlicensed (Cammy keeps running). Do this before moving the license to another machine.",
      confirmLabel: "Remove license",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await api.removeLicense();
      await load();
      toast.success("License removed from this machine.");
    } catch (e) {
      toast.error(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card" data-settings-group="license">
      <h2>License</h2>
      <StatusLine ent={info.entitlement} trialDays={info.trial_days} />

      {licensed ? (
        <div className="row" style={{ alignItems: "center" }}>
          <span className="callout-ico" style={{ color: "var(--success)" }}>
            <IconCheck size={16} />
          </span>
          <span className="muted" style={{ flex: 1 }}>
            This machine is activated. Remove the license here before moving it to another
            computer.
          </span>
          <button type="button" className="btn btn-danger" disabled={busy} onClick={remove}>
            Remove license
          </button>
        </div>
      ) : (
        <>
          <p className="muted">
            Paste the license key from your purchase email to activate this machine. Keys
            verify locally — no internet connection is required.
          </p>
          <div className="row">
            <input
              type="text"
              aria-label="License key"
              placeholder="CAMMY-…"
              value={key}
              spellCheck={false}
              autoComplete="off"
              style={{ flex: 1, fontFamily: "var(--font-mono)" }}
              onChange={(e) => setKey(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && activate()}
            />
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || !key.trim()}
              onClick={activate}
            >
              Activate
            </button>
            <a className="btn btn-ghost" href={info.buy_url} target="_blank" rel="noreferrer">
              Buy a license
            </a>
          </div>
        </>
      )}
    </div>
  );
}
