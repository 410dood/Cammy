import { ChangeEvent, FormEvent, useEffect, useState } from "react";
import { AlarmRule, api, ApiToken, ArmMode, AuditEntry, Camera, Capability, ClipShare, DAY_NAMES, fmtBytes, fmtTime, Me, NotifyPref, Occupant, OffsiteStatus, Role, Settings as S, User } from "../api";
import { useToast, useDialog, RelTime, TogglePill, ErrorState, Callout } from "../ui";
import { LicensePane } from "../License";
import { prettyGesture } from "../labels";
import {
  IconProps, IconLogIn, IconBan, IconKey, IconLock, IconTicket, IconTrash,
  IconDownload, IconUpload, IconCheck, IconUser, IconShield, IconAlert, IconMic,
} from "../icons";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

const AUDIT_META: Record<string, { label: string; Icon: (p: IconProps) => JSX.Element; cls: string }> = {
  login_success: { label: "login", Icon: IconLogIn, cls: "ok" },
  login_failed: { label: "failed login", Icon: IconBan, cls: "danger" },
  password_set: { label: "password set", Icon: IconKey, cls: "" },
  password_cleared: { label: "password cleared", Icon: IconLock, cls: "warn" },
  token_created: { label: "token created", Icon: IconTicket, cls: "" },
  token_revoked: { label: "token revoked", Icon: IconTrash, cls: "warn" },
  user_created: { label: "user created", Icon: IconUser, cls: "" },
  user_deleted: { label: "user deleted", Icon: IconTrash, cls: "warn" },
  user_role_changed: { label: "role changed", Icon: IconUser, cls: "warn" },
  user_password_changed: { label: "user password reset", Icon: IconKey, cls: "" },
  "2fa_enabled": { label: "two-factor enabled", Icon: IconShield, cls: "ok" },
  "2fa_disabled": { label: "two-factor disabled", Icon: IconShield, cls: "warn" },
  "2fa_recovery_used": { label: "recovery code used", Icon: IconKey, cls: "warn" },
  "2fa_reset": { label: "two-factor reset (admin)", Icon: IconShield, cls: "warn" },
};

// Curated security sounds → the EXACT AudioSet display names YAMNet emits
// (audio.rs matches these case-insensitively). A chip is "on" when all its
// values are in Settings.audio_labels; toggling adds/removes them together.
const AUDIO_SOUNDS: { label: string; values: string[] }[] = [
  { label: "Glass breaking", values: ["Glass", "Shatter"] },
  { label: "Gunshot", values: ["Gunshot, gunfire"] },
  { label: "Scream", values: ["Screaming"] },
  { label: "Smoke / fire alarm", values: ["Smoke detector, smoke alarm", "Fire alarm"] },
  { label: "Siren", values: ["Siren"] },
  { label: "Car alarm", values: ["Car alarm"] },
  { label: "Alarm / buzzer", values: ["Alarm"] },
  { label: "Dog bark", values: ["Bark"] },
  { label: "Cat meow", values: ["Meow", "Cat"] },
  { label: "Doorbell", values: ["Doorbell"] },
  { label: "Knock", values: ["Knock"] },
  { label: "Baby cry", values: ["Baby cry, infant cry"] },
  { label: "Child crying", values: ["Crying, sobbing"] },
];

function AuditCard() {
  const [rows, setRows] = useState<AuditEntry[]>([]);
  useEffect(() => {
    api.audit(100).then(setRows).catch(() => {});
  }, []);
  if (rows.length === 0) return null;
  return (
    <div className="card" data-settings-group="security">
      <h2>Recent security activity</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Logins, password changes, and API-token changes — most recent first. Useful for
        spotting unexpected access on a WAN-exposed server.
      </p>
      <div className="table-scroll">
      <table style={{ width: "100%", borderCollapse: "collapse" }}>
        <tbody>
          {rows.map((r) => {
            const m = AUDIT_META[r.action];
            return (
              <tr key={r.id}>
                <td>
                  {m ? (
                    <span className={`badge ${m.cls}`}>
                      <m.Icon size={13} /> {m.label}
                    </span>
                  ) : (
                    r.action
                  )}
                </td>
                <td className="muted clock">{fmtTime(r.ts)}</td>
                <td className="muted">{r.ip ?? ""}</td>
                <td className="muted">{r.detail ?? ""}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      </div>
    </div>
  );
}

// base64url (no padding) → Uint8Array, for the VAPID applicationServerKey.
function urlBase64ToUint8Array(base64url: string): Uint8Array {
  const padding = "=".repeat((4 - (base64url.length % 4)) % 4);
  const base64 = (base64url + padding).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(base64);
  const arr = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) arr[i] = raw.charCodeAt(i);
  return arr;
}

function PushCard({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const supported =
    "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
  const [subscribed, setSubscribed] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!supported) return;
    navigator.serviceWorker.ready
      .then((reg) => reg.pushManager.getSubscription())
      .then((sub) => {
        setSubscribed(!!sub);
        // Self-heal ownership (P2.11): re-register any existing browser
        // subscription so the server stamps it with the current user_id. This is
        // an idempotent upsert by endpoint, so it just refreshes the row — it
        // re-owns anonymous subs left after the one-time server-side reset,
        // without the user toggling push off/on.
        if (sub) api.pushSubscribe(sub.toJSON()).catch(() => {});
      })
      .catch(() => {});
  }, [supported]);

  const enable = async () => {
    setBusy(true);
    try {
      const perm = await Notification.requestPermission();
      if (perm !== "granted") {
        onError("Notification permission was not granted.");
        return;
      }
      const { public_key } = await api.pushVapid();
      const reg = await navigator.serviceWorker.ready;
      const sub = await reg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: urlBase64ToUint8Array(public_key).buffer as ArrayBuffer,
      });
      await api.pushSubscribe(sub.toJSON());
      setSubscribed(true);
      toast.success("Push notifications enabled on this device.");
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const disable = async () => {
    setBusy(true);
    try {
      const reg = await navigator.serviceWorker.ready;
      const sub = await reg.pushManager.getSubscription();
      if (sub) {
        await api.pushUnsubscribe(sub.endpoint).catch(() => {});
        await sub.unsubscribe();
      }
      setSubscribed(false);
      toast.success("Push notifications disabled on this device.");
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const test = async () => {
    try {
      const r = await api.pushTest();
      toast.success(
        `Test push sent to ${r.sent} device${r.sent === 1 ? "" : "s"}` +
          (r.failed ? ` (${r.failed} failed)` : ""),
      );
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <div className="card" data-settings-group="modes">
      <h2>Push notifications</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Get native notifications on this device when alarms fire or a camera goes offline — even
        when Cammy isn&apos;t open. Encrypted and sent straight from your own server, with no
        third-party push service or account.
      </p>
      {!supported ? (
        <p className="muted">This browser doesn&apos;t support Web Push.</p>
      ) : (
        <div className="row">
          <span className={`pill ${subscribed ? "on" : ""}`}>{subscribed ? "enabled" : "off"}</span>
          {subscribed ? (
            <>
              <button type="button" className="danger" disabled={busy} onClick={disable}>
                Disable
              </button>
              <button type="button" disabled={busy} onClick={test}>
                Send test
              </button>
            </>
          ) : (
            <button type="button" className="primary" disabled={busy} onClick={enable}>
              Enable on this device
            </button>
          )}
        </div>
      )}
    </div>
  );
}

// Auth state (`enabled`) is owned by the Settings page — it also drives the
// page-level passwordless banner above the tabs — and updated via onEnabled.
function RemoteAccessCard({
  enabled,
  onEnabled,
  onError,
}: {
  enabled: boolean | null; // null = still loading
  onEnabled: (on: boolean) => void;
  onError: (e: string) => void;
}) {
  const toast = useToast();
  const [pw, setPw] = useState("");

  const apply = async (password: string) => {
    try {
      const r = await api.setPassword(password);
      onEnabled(r.enabled);
      setPw("");
      toast.success(r.enabled ? "Password set — other devices must now log in." : "Password cleared.");
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <div className="card" data-settings-group="security">
      <h2>Remote access</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        When a password is set, other devices on your network must log in. This computer
        (localhost / the desktop app) is always exempt.
      </p>
      {enabled === false && (
        <Callout tone="warn">
          <b>No password set</b> — anyone who can reach this server has full access. Set a password
          before exposing it beyond this computer.
        </Callout>
      )}
      <div className="row">
        <span className={`pill ${enabled ? "on" : ""}`}>{enabled ? "protected" : "open"}</span>
        <input
          type="password"
          autoComplete="new-password"
          placeholder="new password"
          value={pw}
          onChange={(e) => setPw(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && pw.length >= 6 && apply(pw)}
        />
        <button type="button" className="btn btn-primary" disabled={pw.length < 6} onClick={() => apply(pw)}>
          Set password
        </button>
        {enabled && (
          <button type="button" className="btn btn-danger" onClick={() => apply("")}>
            Clear
          </button>
        )}
      </div>
      <small className="muted" style={{ display: "block", marginTop: 6 }}>At least 6 characters.</small>
    </div>
  );
}

function TokensCard({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const dialog = useDialog();
  const [tokens, setTokens] = useState<ApiToken[]>([]);
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>("operator");
  const [fresh, setFresh] = useState<{ name: string; role: Role; token: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const load = () => {
    api.tokens().then(setTokens).catch(() => {});
  };
  useEffect(load, []);

  const create = async () => {
    const n = name.trim();
    if (!n || busy) return;
    setBusy(true);
    try {
      const r = await api.createToken(n, role);
      setFresh({ name: r.name, role: r.role, token: r.token });
      toast.success(`Token “${r.name}” created`);
      setName("");
      load();
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const remove = async (id: number) => {
    const ok = await dialog.confirm({
      title: "Revoke this token?",
      body: "Anything using it (scripts, Home Assistant, integrations) will immediately lose access.",
      confirmLabel: "Revoke",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.deleteToken(id);
      toast.success("Token revoked");
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <div className="card" data-settings-group="security">
      <h2>API tokens</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Bearer tokens let scripts and integrations (Home Assistant, MQTT automations) call the
        API from another machine without logging in — send{" "}
        <code>Authorization: Bearer &lt;token&gt;</code>. Each token is <b>scoped to a role</b>:{" "}
        <b>viewer</b> (read-only), <b>operator</b> (read + manage cameras/settings/alarms), or{" "}
        <b>admin</b> (also backup/restore). Pick the least privilege the integration needs, keep it
        secret, and revoke any that leak. (No token — at any role — can change a password or
        create/revoke tokens; those need an interactive login, so a leaked token can't lock you out.)
      </p>
      <div className="row">
        <input
          type="text"
          placeholder="token name (e.g. home-assistant)"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && create()}
        />
        <select value={role} onChange={(e) => setRole(e.target.value as Role)} aria-label="token role">
          <option value="viewer">viewer</option>
          <option value="operator">operator</option>
          <option value="admin">admin</option>
        </select>
        <button type="button" className="primary" disabled={busy || !name.trim()} onClick={create}>
          {busy ? "Creating…" : "Create token"}
        </button>
      </div>
      {fresh && (
        <div
          className="row"
          style={{ marginTop: 8, flexDirection: "column", alignItems: "flex-start", gap: 4 }}
        >
          <span style={{ color: "var(--ok)" }}>
            New <b>{fresh.role}</b> token “{fresh.name}” — copy it now, it won’t be shown again:
          </span>
          <div className="row" style={{ gap: 8, alignItems: "center", width: "100%" }}>
            <code style={{ userSelect: "all", wordBreak: "break-all", flex: 1 }}>{fresh.token}</code>
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() =>
                navigator.clipboard?.writeText(fresh.token).then(
                  () => toast.success("Token copied"),
                  () => {},
                )
              }
            >
              Copy
            </button>
          </div>
        </div>
      )}
      {tokens.length > 0 && (
        <div className="table-scroll">
        <table style={{ marginTop: 12, width: "100%", borderCollapse: "collapse" }}>
          <tbody>
            {tokens.map((t) => (
              <tr key={t.id}>
                <td>
                  <b>{t.name}</b> <span className={`role-pill role-${t.role}`}>{t.role}</span>
                </td>
                <td className="muted">created <RelTime ts={t.created_ts} /></td>
                <td className="muted">
                  {t.last_used_ts ? <RelTime ts={t.last_used_ts} prefix="last used " /> : "never used"}
                </td>
                <td style={{ textAlign: "right" }}>
                  <button type="button" className="danger" onClick={() => remove(t.id)}>
                    Revoke
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      )}
    </div>
  );
}

// Shareable clip links (P2.7) — list active no-login links + revoke them early.
function SharesCard({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const dialog = useDialog();
  const [shares, setShares] = useState<ClipShare[]>([]);
  const load = () => api.shares().then(setShares).catch((e) => onError(String(e)));
  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const now = Date.now() / 1000;
  // Only active links are actionable; expired ones auto-prune server-side.
  const active = shares.filter((s) => !s.revoked && s.expires_ts > now);
  const revoke = async (s: ClipShare) => {
    const ok = await dialog.confirm({
      title: "Revoke this share link?",
      body: "Anyone holding the link will immediately lose access to the clip.",
      confirmLabel: "Revoke",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.revokeShare(s.id);
      toast.success("Link revoked");
      load();
    } catch (e) {
      toast.error(String(e));
    }
  };
  return (
    <div className="card" data-settings-group="security">
      <h2>Shared clip links</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        No-login links to a single event's clip, created with the <b>Share</b> button on the Events
        page. They expire on their own; revoke one here to cut access early.
      </p>
      {active.length === 0 ? (
        <p className="muted">No active share links.</p>
      ) : (
        <div className="table-scroll">
          <table style={{ width: "100%", borderCollapse: "collapse" }}>
            <tbody>
              {active.map((s) => (
                <tr key={s.id}>
                  <td>
                    <b style={{ textTransform: "capitalize" }}>{s.label ?? "event"}</b>{" "}
                    <span className="muted">· {s.camera ?? ""} · event {s.event_id}</span>
                  </td>
                  <td className="muted">
                    <RelTime ts={s.expires_ts} prefix="expires " />
                  </td>
                  <td style={{ textAlign: "right" }}>
                    <button type="button" className="danger" onClick={() => revoke(s)}>
                      Revoke
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

// Self-service password change — only shown to a logged-in *named* account
// (loopback / legacy / token admins manage the shared password elsewhere).
function AccountCard({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const [me, setMe] = useState<Me | null>(null);
  const [oldPw, setOldPw] = useState("");
  const [newPw, setNewPw] = useState("");
  const [busy, setBusy] = useState(false);
  const [email, setEmail] = useState("");
  const [emailBusy, setEmailBusy] = useState(false);

  useEffect(() => {
    api
      .me()
      .then((m) => {
        setMe(m);
        setEmail(m.email ?? "");
      })
      .catch(() => {});
  }, []);

  if (!me || !me.named) return null;

  const saveEmail = async () => {
    setEmailBusy(true);
    try {
      const r = await api.setMyEmail(email.trim());
      setEmail(r.email ?? "");
      toast.success(r.email ? "Notification email saved" : "Notification email cleared");
    } catch (e) {
      onError(String(e));
    } finally {
      setEmailBusy(false);
    }
  };

  const submit = async () => {
    if (newPw.length < 6) {
      onError("new password must be at least 6 characters");
      return;
    }
    setBusy(true);
    try {
      await api.changeMyPassword(oldPw, newPw);
      setOldPw("");
      setNewPw("");
      toast.success("Password changed");
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card" data-settings-group="security">
      <h2>Your account</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Signed in as <b>{me.username}</b>{" "}
        <span className={`role-pill role-${me.role}`}>{me.role}</span>. Change your own password
        here — you’ll need your current one.
      </p>
      <div className="row">
        <input
          type="password"
          autoComplete="current-password"
          placeholder="current password"
          value={oldPw}
          onChange={(e) => setOldPw(e.target.value)}
        />
        <input
          type="password"
          autoComplete="new-password"
          placeholder="new password (min 6)"
          value={newPw}
          onChange={(e) => setNewPw(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && submit()}
        />
        <button
          type="button"
          className="primary"
          disabled={busy || !oldPw || newPw.length < 6}
          onClick={submit}
        >
          Change password
        </button>
      </div>
      <p className="muted" style={{ margin: "14px 0 4px" }}>
        Notification email — where your own alerts are sent (when an admin has set up
        email). Leave blank for none.
      </p>
      <div className="row">
        <input
          type="email"
          autoComplete="off"
          placeholder="name@example.com"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && saveEmail()}
          style={{ flex: 1, minWidth: 220 }}
        />
        <button type="button" className="btn btn-ghost" disabled={emailBusy} onClick={saveEmail}>
          Save email
        </button>
      </div>
    </div>
  );
}

// Self-service TOTP two-factor enrollment, for the caller's own credential —
// a named user account or, on the local box / legacy login, the shared password.
function TwoFactorCard({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const dialog = useDialog();
  const [status, setStatus] = useState<{
    enabled: boolean;
    pending: boolean;
    scope: "user" | "shared";
    account: string;
  } | null>(null);
  const [setup, setSetup] = useState<{ secret: string; otpauth_uri: string } | null>(null);
  const [code, setCode] = useState("");
  const [recovery, setRecovery] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);

  const load = () => api.twofaStatus().then(setStatus).catch(() => {});
  useEffect(() => {
    load();
  }, []);

  if (!status) return null;

  const begin = async () => {
    setBusy(true);
    try {
      const s = await api.twofaSetup();
      setSetup({ secret: s.secret, otpauth_uri: s.otpauth_uri });
      setCode("");
      setRecovery(null);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const enable = async () => {
    setBusy(true);
    try {
      const r = await api.twofaEnable(code.trim());
      setRecovery(r.recovery_codes);
      setSetup(null);
      setCode("");
      toast.success("Two-factor enabled");
      load();
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const disable = async () => {
    const c = await dialog.prompt({
      title: "Disable two-factor",
      label: "Current authenticator code or a recovery code (leave blank on the server's own machine)",
    });
    if (c === null) return;
    setBusy(true);
    try {
      await api.twofaDisable(c.trim());
      toast.success("Two-factor disabled");
      setRecovery(null);
      load();
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const copyRecovery = () => {
    if (!recovery) return;
    navigator.clipboard?.writeText(recovery.join("\n")).then(
      () => toast.success("Recovery codes copied"),
      () => {},
    );
  };

  return (
    <div className="card" data-settings-group="security">
      <h2>Two-factor authentication</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Require a one-time code from an authenticator app (Google Authenticator, Aegis, 1Password, …)
        on top of {status.scope === "user" ? "your password" : "the shared password"} when logging in
        remotely.{" "}
        {status.scope === "shared" &&
          "Applies to remote logins using the shared password — this computer (localhost) is always exempt, so you can't lock yourself out locally."}
      </p>

      {recovery && (
        <div className="card" style={{ background: "var(--surface-hover)", marginBottom: 0 }}>
          <b>Save your recovery codes</b>
          <p className="muted" style={{ margin: "4px 0" }}>
            Each can be used once if you lose your authenticator. They won't be shown again — store
            them somewhere safe.
          </p>
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "1fr 1fr",
              gap: 6,
              fontFamily: "var(--font-mono)",
              margin: "8px 0",
            }}
          >
            {recovery.map((c) => (
              <span key={c}>{c}</span>
            ))}
          </div>
          <div className="row">
            <button type="button" className="btn btn-ghost" onClick={copyRecovery}>
              Copy codes
            </button>
            <button type="button" className="btn btn-primary" onClick={() => setRecovery(null)}>
              I've saved them
            </button>
          </div>
        </div>
      )}

      {!recovery && status.enabled && (
        <div className="row" style={{ alignItems: "center" }}>
          <span className="badge ok">
            <IconShield size={14} /> Enabled
          </span>
          <button type="button" className="btn btn-danger" disabled={busy} onClick={disable}>
            Disable two-factor
          </button>
        </div>
      )}

      {!recovery && !status.enabled && !setup && (
        <button type="button" className="btn btn-primary" disabled={busy} onClick={begin}>
          <IconShield size={15} /> Set up two-factor
        </button>
      )}

      {!recovery && !status.enabled && setup && (
        <div>
          <p className="muted" style={{ marginBottom: 4 }}>
            1. In your authenticator app, add an account using <b>“enter a setup key”</b> and type in
            this key (or import the otpauth link below):
          </p>
          <div
            style={{
              fontFamily: "var(--font-mono)",
              fontSize: "var(--text-md)",
              letterSpacing: 1,
              wordBreak: "break-all",
              padding: "8px 10px",
              background: "var(--surface-hover)",
              borderRadius: "var(--radius-sm)",
            }}
          >
            {setup.secret}
          </div>
          <details style={{ margin: "6px 0" }}>
            <summary className="muted" style={{ cursor: "pointer" }}>
              Show otpauth link
            </summary>
            <div className="row" style={{ gap: 8, alignItems: "center", marginTop: 6 }}>
              <code style={{ wordBreak: "break-all", fontSize: "var(--text-sm)", flex: 1 }}>{setup.otpauth_uri}</code>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() =>
                  navigator.clipboard?.writeText(setup.otpauth_uri).then(
                    () => toast.success("Link copied"),
                    () => {},
                  )
                }
              >
                Copy
              </button>
            </div>
          </details>
          <p className="muted" style={{ marginBottom: 4 }}>
            2. Enter the 6-digit code it shows to confirm:
          </p>
          <div className="row">
            <input
              type="text"
              inputMode="numeric"
              autoComplete="one-time-code"
              placeholder="123456"
              value={code}
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && code.trim().length >= 6 && enable()}
            />
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || code.trim().length < 6}
              onClick={enable}
            >
              Enable
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              onClick={() => {
                setSetup(null);
                setCode("");
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function BackupCard({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const dialog = useDialog();
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);

  const onFile = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = ""; // let the user re-pick the same file later
    if (!file) return;
    const ok = await dialog.confirm({
      title: "Restore configuration?",
      body: "Settings are replaced. Cameras and alarms whose names already exist are kept as-is.",
      confirmLabel: "Restore",
    });
    if (!ok) return;
    setBusy(true);
    setMsg("");
    let backup: unknown;
    try {
      backup = JSON.parse(await file.text());
    } catch {
      onError("That file isn't a valid Cammy backup — it isn't valid JSON. Pick the .json file you exported with “Download backup”.");
      setBusy(false);
      return;
    }
    try {
      const r = await api.restore(backup);
      toast.success("Configuration restored — reload to see changes");
      setMsg(
        `Restored — ${r.cameras_added} camera(s) added, ${r.cameras_skipped} skipped, ${r.alarms_added} alarm(s) added, settings applied. Reload to see changes.`,
      );
    } catch (err) {
      onError(`Couldn't restore that backup. (${errMsg(err)})`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card" data-settings-group="recording">
      <h2>Backup &amp; restore</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Export your configuration — cameras, settings and alarm rules — to a JSON file to move to
        another machine. Recordings, events and enrolled faces are <b>not</b> included. Restore is
        additive: settings are replaced, but a camera/alarm whose name already exists is left
        untouched.
      </p>
      <div className="callout callout-warn" role="note">
        <span className="callout-ico"><IconAlert size={16} /></span>
        <div>The backup file contains your camera credentials in clear text — store it somewhere private.</div>
      </div>
      <div className="row" style={{ alignItems: "center" }}>
        <a className="btn btn-ghost" href="/api/backup" download="cammy-backup.json">
          <IconDownload size={15} /> Download backup
        </a>
        <label className="btn btn-ghost" style={{ cursor: busy ? "wait" : "pointer" }}>
          <IconUpload size={15} /> Restore from file…
          <input
            type="file"
            accept="application/json,.json"
            style={{ display: "none" }}
            disabled={busy}
            onChange={onFile}
          />
        </label>
        {msg && <span style={{ color: "var(--ok)" }}>{msg}</span>}
      </div>
    </div>
  );
}

/// Live offsite-backup sync readout. Polls every 5s; read-only (no secrets).
function OffsiteStatusReadout() {
  const [st, setSt] = useState<OffsiteStatus | null>(null);
  useEffect(() => {
    let live = true;
    const poll = () => api.offsiteStatus().then((s) => live && setSt(s)).catch(() => {});
    poll();
    const t = setInterval(() => { if (!document.hidden) poll(); }, 5000);
    return () => {
      live = false;
      clearInterval(t);
    };
  }, []);
  if (!st || !st.enabled) return null;
  return (
    <div className="row" style={{ gap: 16, marginTop: 4, flexWrap: "wrap", fontSize: 13 }}>
      <span className="muted">
        Status:{" "}
        {!st.configured ? (
          <b style={{ color: "var(--warn)" }}>not fully configured</b>
        ) : st.backlog > 0 ? (
          <b style={{ color: "var(--warn)" }}>{st.backlog} segment(s) pending</b>
        ) : st.done > 0 ? (
          <b style={{ color: "var(--ok)" }}>up to date</b>
        ) : (
          // Configured + nothing pending but nothing uploaded yet: don't claim a
          // confident green — the creds/bucket aren't proven until a real PUT.
          <span>waiting for the first sealed segment…</span>
        )}
      </span>
      <span className="muted">{st.done} uploaded</span>
      <span className="muted">{fmtBytes(st.bytes_total)} total</span>
      {st.last_success_ts && (
        <span className="muted">
          last <RelTime ts={st.last_success_ts} />
        </span>
      )}
      {st.skipped + st.gaveup > 0 && (
        <span
          style={{ color: "var(--warn)" }}
          title="Segments removed locally (retention) before they could be backed up, or abandoned after repeated failures. Increase retention or check the backup target."
        >
          {st.skipped + st.gaveup} dropped before backup
        </span>
      )}
      {st.last_error && (
        <span style={{ color: "var(--danger)" }} title={st.last_error}>
          last error: {st.last_error.slice(0, 80)}
        </span>
      )}
    </div>
  );
}

function UsersCard({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const dialog = useDialog();
  const [me, setMe] = useState<Me | null>(null);
  const [users, setUsers] = useState<User[]>([]);
  const [name, setName] = useState("");
  const [pw, setPw] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [cams, setCams] = useState<Camera[]>([]);
  const [editing, setEditing] = useState<number | null>(null);
  const [editIds, setEditIds] = useState<Set<number>>(new Set());
  // P2.11 per-user notification matrix panel.
  const [alarms, setAlarms] = useState<AlarmRule[]>([]);
  const [notifFor, setNotifFor] = useState<number | null>(null);
  // Resolved WYSIWYG state, keyed `${rule_id}:${channel}` → on/off. rule_id 0 is
  // the per-user default row for rules not set individually.
  const [notifState, setNotifState] = useState<Record<string, boolean>>({});
  const [notifEmail, setNotifEmail] = useState("");
  // Whether an SMTP server is configured — email toggles are inert without it.
  const [smtpConfigured, setSmtpConfigured] = useState(true);

  const load = () => {
    api.me().then(setMe).catch(() => {});
    api.users().then(setUsers).catch(() => {});
    api.cameras().then(setCams).catch(() => {});
    api.alarms().then(setAlarms).catch(() => {});
    api
      .settings()
      .then((s) => setSmtpConfigured(!!s.smtp_url?.trim()))
      .catch(() => {});
  };
  useEffect(load, []);

  const CHANNELS: ("push" | "email")[] = ["push", "email"];

  const openNotif = async (u: User) => {
    if (notifFor === u.id) {
      setNotifFor(null);
      return;
    }
    setEditing(null); // don't stack the two expanders
    try {
      const prefs = await api.userNotifyPrefs(u.id);
      const explicit = new Map(prefs.map((p) => [`${p.rule_id}:${p.channel}`, p.enabled]));
      // Resolve each cell (exact rule row → user default row → on) into an
      // explicit grid, so saving writes an unambiguous full set.
      const resolve = (ruleId: number, ch: "push" | "email") => {
        const exact = explicit.get(`${ruleId}:${ch}`);
        if (exact !== undefined) return exact;
        const def = explicit.get(`0:${ch}`);
        if (def !== undefined) return def;
        return true;
      };
      const st: Record<string, boolean> = {};
      for (const ch of CHANNELS) {
        st[`0:${ch}`] = resolve(0, ch);
        for (const a of alarms) st[`${a.id}:${ch}`] = resolve(a.id, ch);
      }
      setNotifState(st);
      setNotifEmail(u.email ?? "");
      setNotifFor(u.id);
    } catch (e) {
      onError(String(e));
    }
  };
  const toggleNotif = (ruleId: number, ch: "push" | "email") => {
    const key = `${ruleId}:${ch}`;
    setNotifState((prev) => ({ ...prev, [key]: !(prev[key] ?? true) }));
  };
  const saveNotif = async (u: User) => {
    try {
      // Persist the email (empty clears) then the full pref grid.
      await api.patchUser(u.id, { email: notifEmail.trim() || null });
      const prefs: NotifyPref[] = Object.entries(notifState).map(([key, enabled]) => {
        const [rid, channel] = key.split(":");
        return { user_id: u.id, rule_id: Number(rid), channel: channel as "push" | "email", enabled };
      });
      await api.setUserNotifyPrefs(u.id, prefs);
      toast.success(`Saved notification settings for ${u.username}`);
      setNotifFor(null);
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  const openScope = async (u: User) => {
    if (editing === u.id) {
      setEditing(null);
      return;
    }
    setNotifFor(null); // don't stack the two expanders
    try {
      const ids = await api.userCameras(u.id);
      setEditIds(new Set(ids));
      setEditing(u.id);
    } catch (e) {
      onError(String(e));
    }
  };
  const toggleScope = (cid: number) => {
    setEditIds((prev) => {
      const next = new Set(prev);
      if (next.has(cid)) next.delete(cid);
      else next.add(cid);
      return next;
    });
  };
  const saveScope = async (u: User) => {
    try {
      await api.setUserCameras(u.id, [...editIds]);
      toast.success(
        editIds.size === 0
          ? `${u.username} can now see all cameras`
          : `${u.username} restricted to ${editIds.size} camera(s)`
      );
      setEditing(null);
    } catch (e) {
      onError(String(e));
    }
  };

  // Only admins manage users (the backend gates it too).
  if (!me || me.role !== "admin") return null;

  const create = async () => {
    if (!name.trim() || pw.length < 6) return;
    try {
      await api.createUser({ username: name.trim(), password: pw, role });
      setName("");
      setPw("");
      setRole("viewer");
      toast.success("User created");
      load();
    } catch (e) {
      onError(String(e));
    }
  };
  const changeRole = async (u: User, r: Role) => {
    try {
      await api.patchUser(u.id, { role: r });
      toast.success(`${u.username} is now ${r}`);
      load();
    } catch (e) {
      onError(String(e));
    }
  };
  const resetPw = async (u: User) => {
    const np = await dialog.prompt({
      title: `Reset password for ${u.username}`,
      label: "New password (min 6 chars)",
    });
    if (np === null) return;
    if (np.length < 6) {
      toast.error("Password must be at least 6 characters");
      return;
    }
    try {
      await api.patchUser(u.id, { password: np });
      toast.success("Password reset — all sessions logged out");
    } catch (e) {
      onError(String(e));
    }
  };
  const reset2fa = async (u: User) => {
    if (
      !(await dialog.confirm({
        title: `Reset two-factor for ${u.username}?`,
        body: "Clears their authenticator + recovery codes so they can sign in with just their password and re-enroll. Use only when they've lost their device.",
        confirmLabel: "Reset 2FA",
        danger: true,
      }))
    )
      return;
    try {
      await api.patchUser(u.id, { disable_2fa: true });
      toast.success("Two-factor reset — that user can log in with their password");
    } catch (e) {
      onError(String(e));
    }
  };
  const remove = async (u: User) => {
    if (
      !(await dialog.confirm({
        title: `Delete ${u.username}?`,
        body: "Removes this account and signs them out everywhere. This can't be undone.",
        confirmLabel: "Delete",
        danger: true,
      }))
    )
      return;
    try {
      await api.deleteUser(u.id);
      toast.success("User deleted");
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  const RoleOptions = () => (
    <>
      <option value="viewer">viewer</option>
      <option value="operator">operator</option>
      <option value="admin">admin</option>
    </>
  );

  return (
    <div className="card" data-settings-group="security">
      <h2>Users &amp; roles</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Named accounts, each with a role: <b>admin</b> (full control, incl. users),{" "}
        <b>operator</b> (manage cameras, settings, alarms), <b>viewer</b> (read-only + live).
        This computer (localhost) and the legacy single password always have admin, so you can
        never lock yourself out locally.
      </p>
      <div className="row">
        <input type="text" autoComplete="off" placeholder="username" value={name} onChange={(e) => setName(e.target.value)} />
        <input
          type="password"
          autoComplete="new-password"
          placeholder="password (min 6)"
          value={pw}
          onChange={(e) => setPw(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && create()}
        />
        <select value={role} onChange={(e) => setRole(e.target.value as Role)} aria-label="Role">
          <RoleOptions />
        </select>
        <button
          type="button"
          className="btn btn-primary"
          disabled={!name.trim() || pw.length < 6}
          onClick={create}
        >
          Add user
        </button>
      </div>
      {users.length > 0 && (
        <div className="table-scroll">
        <table style={{ marginTop: 12, width: "100%", borderCollapse: "collapse" }}>
          <tbody>
            {users.map((u) => (
              <tr key={u.id}>
                <td>
                  <b>{u.username}</b>
                  {me.username === u.username && <span className="muted"> (you)</span>}
                </td>
                <td>
                  <select
                    value={u.role}
                    onChange={(e) => changeRole(u, e.target.value as Role)}
                    aria-label={`Role for ${u.username}`}
                  >
                    <RoleOptions />
                  </select>
                </td>
                <td className="muted">created <RelTime ts={u.created_ts} /></td>
                <td style={{ textAlign: "right" }}>
                  <div className="ev-actions" style={{ justifyContent: "flex-end" }}>
                    {u.role !== "admin" && (
                      <button
                        type="button"
                        className={`btn ev-act ${editing === u.id ? "btn-primary" : "btn-ghost"}`}
                        onClick={() => openScope(u)}
                        title="Restrict which cameras this user can see"
                      >
                        Cameras
                      </button>
                    )}
                    <button
                      type="button"
                      className={`btn ev-act ${notifFor === u.id ? "btn-primary" : "btn-ghost"}`}
                      onClick={() => openNotif(u)}
                      title="Choose which alerts reach this person, and their email"
                    >
                      Notifications
                    </button>
                    <button type="button" className="btn btn-ghost ev-act" onClick={() => resetPw(u)}>
                      Reset password
                    </button>
                    <button type="button" className="btn btn-ghost ev-act" onClick={() => reset2fa(u)}>
                      Reset 2FA
                    </button>
                    <button type="button" className="btn btn-danger ev-act" onClick={() => remove(u)}>
                      Delete
                    </button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      )}
      {editing !== null && (
        <div className="card" style={{ background: "var(--surface-hover)", marginTop: 12, marginBottom: 0 }}>
          <b>Camera access for {users.find((u) => u.id === editing)?.username}</b>
          <p className="muted" style={{ margin: "4px 0" }}>
            Tick the cameras this user may see (live, events, recordings, snapshots, everything).
            Leave <b>all unticked</b> to give them access to <b>every</b> camera (the default).
            Admins always see all.
          </p>
          {cams.length === 0 ? (
            <p className="muted">No cameras yet.</p>
          ) : (
            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill,minmax(180px,1fr))", gap: 6 }}>
              {cams.map((c) => (
                <label key={c.id} className="toggle" style={{ gap: 8 }}>
                  <input
                    type="checkbox"
                    checked={editIds.has(c.id)}
                    onChange={() => toggleScope(c.id)}
                  />
                  {c.name}
                  {c.group && <span className="muted"> · {c.group}</span>}
                </label>
              ))}
            </div>
          )}
          <div className="row" style={{ marginTop: 8 }}>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => saveScope(users.find((u) => u.id === editing)!)}
            >
              Save access
            </button>
            <button type="button" className="btn btn-ghost" onClick={() => setEditing(null)}>
              Cancel
            </button>
            <span className="muted">
              {editIds.size === 0 ? "all cameras" : `${editIds.size} selected`}
            </span>
          </div>
        </div>
      )}
      {notifFor !== null && (
        <div className="card" style={{ background: "var(--surface-hover)", marginTop: 12, marginBottom: 0 }}>
          <b>Notifications for {users.find((u) => u.id === notifFor)?.username}</b>
          <p className="muted" style={{ margin: "4px 0" }}>
            Choose which alerts reach this person, and where. Push goes to the browsers
            they’ve turned notifications on for; email goes to the address below. Anything
            left on stays on — turn a row off to mute it for them. Camera access still
            applies: they only get alerts from cameras they can see.
          </p>
          {!smtpConfigured && (
            <Callout tone="warn" style={{ margin: "8px 0" }}>
              Email only sends once the SMTP server is configured in Modes &amp; alerts. The
              toggles below are saved either way, but no email is delivered until then.
            </Callout>
          )}
          <div className="row" style={{ alignItems: "center", margin: "8px 0" }}>
            <label style={{ minWidth: 120 }}>Email for alerts</label>
            <input
              type="email"
              autoComplete="off"
              placeholder="name@example.com (leave blank for none)"
              value={notifEmail}
              onChange={(e) => setNotifEmail(e.target.value)}
              style={{ flex: 1, minWidth: 200 }}
            />
          </div>
          <div className="table-scroll">
            <table style={{ width: "100%", borderCollapse: "collapse" }}>
              <thead>
                <tr>
                  <th style={{ textAlign: "left" }}>Alert</th>
                  <th style={{ width: 90 }}>Push</th>
                  <th style={{ width: 90 }}>Email</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td>
                    <b>Default</b>{" "}
                    <span className="muted">
                      · applies to rules not set individually below, and to any added later
                    </span>
                  </td>
                  {CHANNELS.map((ch) => (
                    <td key={ch} style={{ textAlign: "center" }}>
                      <input
                        type="checkbox"
                        aria-label={`Default ${ch}`}
                        checked={notifState[`0:${ch}`] ?? true}
                        onChange={() => toggleNotif(0, ch)}
                      />
                    </td>
                  ))}
                </tr>
                {alarms.map((a) => (
                  <tr key={a.id}>
                    <td>{a.name}</td>
                    {CHANNELS.map((ch) => (
                      <td key={ch} style={{ textAlign: "center" }}>
                        <input
                          type="checkbox"
                          aria-label={`${a.name} ${ch}`}
                          checked={notifState[`${a.id}:${ch}`] ?? true}
                          onChange={() => toggleNotif(a.id, ch)}
                        />
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {alarms.length === 0 && (
            <p className="muted" style={{ marginTop: 6 }}>
              No alert rules yet — the Default row still governs any you add later.
            </p>
          )}
          <div className="row" style={{ marginTop: 8 }}>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => saveNotif(users.find((u) => u.id === notifFor)!)}
            >
              Save notifications
            </button>
            <button type="button" className="btn btn-ghost" onClick={() => setNotifFor(null)}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// Settings is ~20 self-contained cards; a flat scroll is a wall. Group them into
// a few tab-like filters WITHOUT restructuring the page: each top-level card
// declares its group via `data-settings-group` on its own div, and the switcher
// shows/hides them imperatively. Cards are only HIDDEN (never unmounted), so the
// stateful cards (Push/Account/2FA/Users/Tokens/Audit/Backup/RemoteAccess/Models)
// keep in-flight edits when switching, and the single wrapping <form> + sticky
// save bar are untouched. Not ARIA tabs on purpose: the "panels" are scattered
// cards inside one form, so these are plain pressed/unpressed filter buttons
// (the TogglePill / arm-bar convention) rather than a half-implemented tablist.
type GroupKey = "detection" | "modes" | "security" | "recording" | "license";
const SETTINGS_GROUPS: { key: GroupKey; label: string }[] = [
  { key: "detection", label: "Detection & AI" },
  { key: "modes", label: "Modes & alerts" },
  { key: "security", label: "Access & security" },
  { key: "recording", label: "Recording & backup" },
  { key: "license", label: "License" },
];

// Reveal only the active group's cards. An untagged card is left visible on
// every tab so a new card can't silently disappear. Also called from the save
// path, so validation can reveal an invalid card before focusing it.
function applySettingsGroup(active: GroupKey) {
  const cards = document.querySelectorAll<HTMLElement>(".settings-page > form > .card");
  cards.forEach((c) => {
    const g = c.dataset.settingsGroup;
    c.hidden = !!g && g !== active;
  });
}

// About / help / support surface: version + the links a stuck customer needs.
// Lives under the License tab (the "meta" section). Read-only.
function AboutCard() {
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    api
      .config()
      .then((c) => setVersion(c.version ?? null))
      .catch(() => {});
  }, []);
  return (
    <div className="card" data-settings-group="license">
      <h2>About &amp; help</h2>
      <p className="muted" style={{ marginTop: -4 }}>
        Cammy{version ? ` v${version}` : ""} · local-first NVR · your cameras, your data.
      </p>
      <div className="row" style={{ flexWrap: "wrap", gap: 8 }}>
        <a className="btn btn-ghost" href="https://410dood.github.io/Cammy/" target="_blank" rel="noreferrer">
          Website
        </a>
        <a className="btn btn-ghost" href="https://github.com/410dood/Cammy" target="_blank" rel="noreferrer">
          Documentation
        </a>
        <a
          className="btn btn-ghost"
          href="https://github.com/410dood/Cammy/blob/main/docs/TROUBLESHOOTING.md"
          target="_blank"
          rel="noreferrer"
        >
          Troubleshooting
        </a>
        <a
          className="btn btn-ghost"
          href="https://github.com/410dood/Cammy/issues"
          target="_blank"
          rel="noreferrer"
        >
          Get support
        </a>
      </div>
    </div>
  );
}

// Desktop-shell-only card: launch-at-login. The web UI normally runs in a plain
// browser (or against the headless server), where the OS login item is out of
// reach — so this renders ONLY inside the Tauri webview, detected by the
// injected `window.__TAURI__` global, and talks straight to the autostart
// plugin over IPC (no server round-trip; the setting lives in the OS, not the DB).
function DesktopCard() {
  const tauri = (window as unknown as { __TAURI__?: { core: { invoke: (cmd: string) => Promise<unknown> } } }).__TAURI__;
  const [enabled, setEnabled] = useState<boolean | null>(null);
  useEffect(() => {
    if (!tauri) return;
    tauri.core
      .invoke("plugin:autostart|is_enabled")
      .then((v) => setEnabled(!!v))
      .catch(() => setEnabled(null));
  }, []);
  if (!tauri || enabled === null) return null;
  const toggle = () => {
    const next = !enabled;
    setEnabled(next);
    tauri.core.invoke(next ? "plugin:autostart|enable" : "plugin:autostart|disable").catch(() => {
      setEnabled(!next); // OS rejected it — reflect reality, don't lie
    });
  };
  return (
    <div className="card" data-settings-group="license">
      <h2>Desktop app</h2>
      <div className="row" style={{ alignItems: "center", gap: 10 }}>
        <TogglePill on={enabled} onClick={toggle} ariaLabel="Start Cammy when I sign in">
          Start Cammy when I sign in
        </TogglePill>
      </div>
      <p className="muted" style={{ marginTop: 6 }}>
        Launches Cammy (and recording) automatically at sign-in. Also available from the tray menu.
      </p>
    </div>
  );
}

// Presence / geofence arming (P2.10): a phone or automation reports who's home
// via POST /api/arm; first-in/last-out then arms Away when everyone leaves and
// drops back to Home when anyone returns. No PWA geolocation — pure webhook/API.
function PresenceCard({ onError }: { onError: (e: string) => void }) {
  const dialog = useDialog();
  const toast = useToast();
  const [occupants, setOccupants] = useState<Occupant[] | null>(null);

  const load = () => {
    api.presence().then(setOccupants).catch((e) => onError(errMsg(e)));
  };
  useEffect(load, []);

  const remove = async (o: Occupant) => {
    const ok = await dialog.confirm({
      title: `Forget ${o.name}?`,
      body: "Removes them from presence tracking. Their phone or automation can re-add them by reporting home again.",
      confirmLabel: "Forget",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.deletePresence(o.id);
      toast.success(`Forgot ${o.name}`);
      load();
    } catch (e) {
      onError(errMsg(e));
    }
  };

  return (
    <div className="card" data-settings-group="modes">
      <h2>Presence &amp; geofence arming</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Let a phone or automation (Home Assistant, Tasker, Apple Shortcuts) tell Cammy who’s home,
        so it arms <b>Away</b> when everyone leaves and drops back to <b>Home</b> when someone
        returns. First-in / last-out: while anyone is home the system stays <b>Home</b>; when the
        last person leaves it goes <b>Away</b>. Presence never disarms the system — that stays a
        deliberate choice.
      </p>
      {occupants === null ? (
        <p className="muted">Loading…</p>
      ) : occupants.length === 0 ? (
        <Callout tone="info">
          No one is being tracked yet. Wire up the webhook below from a phone or automation — the
          first “home” report adds the person automatically.
        </Callout>
      ) : (
        <div className="table-scroll">
          <table style={{ marginTop: 4, width: "100%", borderCollapse: "collapse" }}>
            <tbody>
              {occupants.map((o) => (
                <tr key={o.id}>
                  <td>
                    <b>{o.name}</b>
                  </td>
                  <td>
                    <span className={`badge ${o.home ? "ok" : ""}`}>{o.home ? "Home" : "Away"}</span>
                  </td>
                  <td className="muted">
                    <RelTime ts={o.updated_ts} prefix="updated " />
                  </td>
                  <td style={{ textAlign: "right" }}>
                    <button
                      type="button"
                      className="btn btn-ghost"
                      aria-label={`Forget ${o.name}`}
                      title={`Forget ${o.name}`}
                      onClick={() => remove(o)}
                    >
                      <IconTrash />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <p className="muted" style={{ marginTop: 12, marginBottom: 4 }}>
        Report arrivals and departures with a POST to <code>/api/arm</code>:
      </p>
      <pre
        style={{
          fontFamily: "var(--font-mono)",
          fontSize: "var(--text-sm)",
          background: "var(--bg)",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius-sm)",
          padding: "10px 12px",
          overflowX: "auto",
          margin: 0,
        }}
      >
        <code>{`curl -X POST http://<cammy-host>:8080/api/arm \\
  -H "Authorization: Bearer <token>" \\
  -H "Content-Type: application/json" \\
  -d '{"occupant":"Alice","home":true}'`}</code>
      </pre>
      <small className="muted" style={{ display: "block", marginTop: 6 }}>
        Create a Bearer token under <b>Access &amp; security → API tokens</b> (operator role). Send{" "}
        <code>{`"home":false`}</code> when the phone leaves the geofence.
      </small>
    </div>
  );
}

function SettingsTabs({ active, onSelect }: { active: GroupKey; onSelect: (g: GroupKey) => void }) {
  // Apply on every switch, and re-apply when a card mounts later (Users appears
  // once admin loads, 2FA/Account/Audit after their fetches) so an async card
  // doesn't land visible in the wrong tab.
  useEffect(() => {
    applySettingsGroup(active);
    const form = document.querySelector<HTMLElement>(".settings-page > form");
    if (!form) return;
    const obs = new MutationObserver(() => applySettingsGroup(active));
    obs.observe(form, { childList: true });
    return () => obs.disconnect();
  }, [active]);

  return (
    <div className="arm-bar settings-tabs" aria-label="Settings sections">
      {SETTINGS_GROUPS.map((g) => (
        <button
          key={g.key}
          type="button"
          aria-pressed={active === g.key}
          className={`arm-opt ${active === g.key ? "active" : ""}`}
          onClick={() => onSelect(g.key)}
        >
          {g.label}
        </button>
      ))}
    </div>
  );
}

// Flags a public base URL that will silently produce dead tap-through links in
// pushes: http:// (phones often block mixed/insecure links) or a private/LAN host
// (won't resolve when you're away from home). Returns null when it looks fine.
function baseUrlWarning(raw: string): string | null {
  const v = raw.trim();
  if (!v) return null;
  let u: URL;
  try {
    u = new URL(v);
  } catch {
    return "Not a valid URL — include the scheme, e.g. https://nvr.example.com";
  }
  const h = u.hostname;
  const isPrivate =
    h === "localhost" ||
    h.endsWith(".local") ||
    /^127\./.test(h) ||
    /^10\./.test(h) ||
    /^192\.168\./.test(h) ||
    /^172\.(1[6-9]|2\d|3[01])\./.test(h);
  if (u.protocol === "http:")
    return "Uses http:// — phones may block the tap-through clip links; prefer https://.";
  if (isPrivate)
    return "Private/LAN host — clip links in pushes won't open when you're away from home.";
  return null;
}

// Surfaces which optional AI models are actually present, so an enabled feature
// whose model isn't downloaded reads as "not downloaded" instead of silently
// no-op'ing (the dominant silent-failure gap). Read-only; fetches on mount.
/// Live readiness line for the Speech-transcription card: is the model file
/// present, and how many cameras are actually listening? Surfaces the two
/// silent preconditions (model + per-camera audio detection) that otherwise
/// make an enabled toggle do nothing.
function TranscriptionReadiness({ enabled }: { enabled: boolean }) {
  const [modelPresent, setModelPresent] = useState<boolean | null>(null);
  const [cams, setCams] = useState<Camera[] | null>(null);
  useEffect(() => {
    api
      .capabilities()
      .then((r) => setModelPresent(r.features.find((f) => f.key === "transcription")?.present ?? false))
      .catch(() => {});
    api.cameras().then(setCams).catch(() => {});
  }, []);
  if (modelPresent === null && cams === null) return null;
  const listening = (cams ?? []).filter((c) => c.enabled && c.detect_config.audio_detect);
  const total = (cams ?? []).filter((c) => c.enabled);
  const blocked = enabled && (modelPresent === false || (cams !== null && listening.length === 0));
  return (
    <div
      className={blocked ? "callout callout-warn" : undefined}
      role={blocked ? "status" : undefined}
      style={blocked ? { marginTop: 8 } : { marginTop: 8, fontSize: "var(--text-sm)" }}
    >
      {modelPresent !== null && (
        <div className={blocked ? undefined : "muted"}>
          {modelPresent ? (
            <>
              <IconCheck size={13} /> Speech model installed.
            </>
          ) : (
            <>
              Speech model not downloaded yet — transcription won't run until it is. See the{" "}
              <a href="https://github.com/410dood/Cammy#optional-ai-models" target="_blank" rel="noreferrer">
                model download guide
              </a>
              .
            </>
          )}
        </div>
      )}
      {cams !== null && (
        <div className={blocked ? undefined : "muted"}>
          {listening.length > 0 ? (
            <>Listening on {listening.length} of {total.length} cameras (those with audio detection on).</>
          ) : (
            <>
              No camera is listening yet — turn on <b>Audio detection</b> in a camera's
              Detection tuning (Cameras page) to transcribe speech near it.
            </>
          )}
        </div>
      )}
    </div>
  );
}

function ModelsCard() {
  const [caps, setCaps] = useState<Capability[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    api
      .capabilities()
      .then((r) => setCaps(r.features))
      .catch((e) => setErr(String(e)));
  }, []);
  return (
    <div className="card" data-settings-group="detection">
      <h2>Models &amp; capabilities</h2>
      <p className="muted" style={{ marginTop: -4 }}>
        Optional AI features only run when their model file is in the app directory. The
        Windows installer bundles the core models; a feature marked "not downloaded" needs
        its model added — see the{" "}
        <a href="https://github.com/410dood/Cammy#optional-ai-models" target="_blank" rel="noreferrer">
          model download guide
        </a>
        . Models are picked up within a minute of being added.
      </p>
      {err ? (
        <p className="muted">Couldn't load the AI feature list: {err}</p>
      ) : !caps ? (
        <span className="skeleton" style={{ width: "100%", height: 36 }} />
      ) : (
        <div>
          {caps.map((c) => (
            <div
              key={c.key}
              className="row"
              style={{
                justifyContent: "space-between",
                alignItems: "center",
                gap: 12,
                padding: "8px 0",
                borderBottom: "1px solid var(--border)",
              }}
            >
              <div>
                <strong>{c.label}</strong>{" "}
                {c.required && <span className="badge">required</span>}
                <div
                  className="muted"
                  style={{ fontSize: "var(--text-sm)", fontFamily: "ui-monospace, monospace" }}
                >
                  {c.model}
                </div>
              </div>
              {c.present ? (
                <span className="badge ok" style={{ whiteSpace: "nowrap" }}>
                  <IconCheck size={13} /> installed
                </span>
              ) : (
                <span
                  className={`badge ${c.required ? "danger" : "warn"}`}
                  style={{ whiteSpace: "nowrap" }}
                  title="Model file not found — this feature will not run until the model is downloaded to the app directory."
                >
                  not downloaded
                </span>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export default function Settings({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const [s, setS] = useState<S | null>(null);
  const [saved, setSaved] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tab, setTab] = useState<GroupKey>("detection");
  // Whether a remote-access password is set (null = still loading). Owned here
  // (not in RemoteAccessCard) so the page-level banner sees it on every tab.
  const [authEnabled, setAuthEnabled] = useState<boolean | null>(null);
  // Whether OpenVINO (Intel iGPU/NPU) genuinely runs in this build, so the global
  // Accelerator dropdown offers it only when it works (honest gate) — false
  // out-of-the-box.
  const [openvinoAvailable, setOpenvinoAvailable] = useState(false);

  const load = () => {
    setLoadError(null);
    api.settings().then(setS).catch((e) => setLoadError(errMsg(e)));
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(load, []);

  useEffect(() => {
    api.authStatus().then((a) => setAuthEnabled(a.enabled)).catch(() => {});
    api.capabilities().then((r) => setOpenvinoAvailable(!!r.openvino)).catch(() => {});
  }, []);

  // Warn before a refresh/close/navigation-away discards unsaved global edits.
  // (In-app page switches surface the "Unsaved changes" cue on the save bar.)
  useEffect(() => {
    if (!dirty) return;
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);

  if (loadError && !s)
    return (
      <div className="settings-page">
        <h1>Settings</h1>
        <ErrorState what="settings" message={loadError} onRetry={load} />
      </div>
    );

  if (!s)
    return (
      <div className="settings-page" aria-busy="true">
        <h1>Settings</h1>
        {Array.from({ length: 4 }).map((_, i) => (
          <div className="card" key={i}>
            <span className="skeleton" style={{ width: 150, height: 12, marginBottom: 16 }} />
            <span className="skeleton" style={{ width: "100%", height: 36, marginBottom: 10 }} />
            <span className="skeleton" style={{ width: "60%", height: 36 }} />
          </div>
        ))}
      </div>
    );

  const set = (patch: Partial<S>) => {
    setS({ ...s, ...patch });
    setSaved(false);
    setDirty(true);
  };

  const save = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    // The form is noValidate: with cards hidden on other tabs, native submit
    // validation dies with "invalid form control is not focusable" and Save
    // silently does nothing. Instead, validate here — jump to the tab holding
    // the first invalid control, reveal its card, then let the native bubble
    // render on the now-focusable field. Invalid values still never save.
    const form = e.currentTarget;
    if (!form.checkValidity()) {
      const bad = form.querySelector<HTMLInputElement>(":invalid");
      const card = bad?.closest<HTMLElement>("[data-settings-group]");
      const g = card?.dataset.settingsGroup as GroupKey | undefined;
      if (g) {
        setTab(g);
        applySettingsGroup(g); // reveal now — don't race the tab effect
      }
      requestAnimationFrame(() => (bad ?? form).reportValidity());
      return;
    }
    try {
      setS(await api.saveSettings(s));
      setSaved(true);
      setDirty(false);
      toast.success("Settings saved");
    } catch (err) {
      onError(String(err));
    }
  };

  const num = (v: string, fallback: number) => {
    const n = Number(v);
    return Number.isFinite(n) ? n : fallback;
  };

  return (
    <div className="settings-page">
      <h1>Settings</h1>
      {/* Action-required security state stays visible on every tab — the Remote
          access card itself lives behind the Access & security tab. */}
      {authEnabled === false && (
        <Callout tone="warn">
          <b>No password set</b> — anyone who can reach this server has full access.{" "}
          <button type="button" className="btn btn-ghost" onClick={() => setTab("security")}>
            Set a password
          </button>
        </Callout>
      )}
      <SettingsTabs active={tab} onSelect={setTab} />
      <form onSubmit={save} noValidate>
        <ModelsCard />
        <div className="card" data-settings-group="detection">
          <h2>Detection</h2>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: "min(380px, 100%)" }}>
              objects (comma-separated, empty = all)
              <input
                type="text"
                value={s.detect_labels.join(", ")}
                onChange={(e) =>
                  set({
                    detect_labels: e.target.value
                      .split(",")
                      .map((x) => x.trim())
                      .filter(Boolean),
                  })
                }
              />
            </label>
            <label className="field">
              alert objects (shown in the Alerts review tab)
              <input
                type="text"
                value={(s.alert_labels ?? []).join(", ")}
                onChange={(e) =>
                  set({
                    alert_labels: e.target.value
                      .split(",")
                      .map((x) => x.trim())
                      .filter(Boolean),
                  })
                }
              />
            </label>
            <label className="field">
              min confidence (0-1)
              <input
                type="number" step="0.05" min="0" max="1"
                value={s.confidence}
                onChange={(e) => set({ confidence: num(e.target.value, s.confidence) })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                How sure the AI must be before logging an object (0 to 1). Higher means fewer
                false alerts but more misses. 0.4 is a good start.
              </span>
            </label>
            <label className="field">
              motion threshold (0-1)
              <input
                type="number" step="0.005" min="0" max="1"
                value={s.motion_threshold}
                onChange={(e) => set({ motion_threshold: num(e.target.value, s.motion_threshold) })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Fraction of the frame that must change before the AI looks for objects. Lower is
                more sensitive.
              </span>
            </label>
            <label className="field">
              sample interval (ms)
              <input
                type="number" step="100" min="100"
                value={s.poll_ms}
                onChange={(e) => set({ poll_ms: num(e.target.value, s.poll_ms) })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Milliseconds between analyzed frames. Higher is easier on your machine but reacts
                slower.
              </span>
            </label>
            <label className="field">
              time between repeat events (s)
              <input
                type="number" min="0"
                value={s.event_cooldown_secs}
                onChange={(e) => set({ event_cooldown_secs: num(e.target.value, s.event_cooldown_secs) })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Wait this long before logging the same object again, so one visitor isn't fifty
                events.
              </span>
            </label>
            <label
              className="toggle field"
              title="Burn an amber outline of the motion region(s) that tripped the gate onto each detection snapshot (next to the red object boxes), so you can see what actually triggered an event — e.g. wind in the trees vs. the object itself."
            >
              highlight motion on snapshots
              <input
                type="checkbox"
                checked={s.highlight_motion ?? true}
                onChange={() => set({ highlight_motion: !(s.highlight_motion ?? true) })}
              />
            </label>
            <label className="field" title="Which processor runs AI detection (and face/pose). GPU is fastest; CPU is the most compatible fallback.">
              detection accelerator
              <select
                value={
                  s.accelerator === "openvino"
                    ? "openvino"
                    : s.accelerator === "cpu" || s.force_cpu
                    ? "cpu"
                    : "gpu"
                }
                onChange={(e) => {
                  const v = e.target.value;
                  // Mirror the legacy force_cpu flag so older code paths stay
                  // consistent; "" is never selectable here (this is the global
                  // default), so an explicit accelerator is always written.
                  if (v === "gpu") set({ accelerator: "", force_cpu: false });
                  else if (v === "cpu") set({ accelerator: "cpu", force_cpu: true });
                  else if (v === "openvino") set({ accelerator: "openvino", force_cpu: false });
                }}
              >
                <option value="gpu">GPU (best for this OS)</option>
                <option value="cpu">CPU only</option>
                <option value="openvino" disabled={!openvinoAvailable}>
                  OpenVINO (Intel){openvinoAvailable ? "" : " — not available in this build"}
                </option>
              </select>
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                GPU uses this OS's accelerator (DirectML/CoreML/CUDA). Choose CPU if GPU detection
                causes problems — slower but more compatible.
                {!openvinoAvailable && " OpenVINO (Intel iGPU/NPU) needs a build with the Intel EP."}
              </span>
            </label>
            <label className="field">
              detection worker threads (advanced)
              <input
                type="number" min="1" max="8" step="1"
                value={s.detect_workers ?? 1}
                onChange={(e) =>
                  set({ detect_workers: Math.min(8, Math.max(1, Math.round(num(e.target.value, s.detect_workers ?? 1)))) })
                }
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Parallel detection pipelines — raise on a many-camera box so one slow camera can't
                stall the others. Each worker uses its own detector session (more RAM/VRAM). Takes
                effect after a restart.
              </span>
            </label>
            <label className="toggle field">
              face recognition
              <input
                type="checkbox"
                checked={s.face_recognition}
                onChange={() => set({ face_recognition: !s.face_recognition })}
              />
            </label>
            <label className="field">
              face match threshold (0-1)
              <input
                type="number" step="0.05" min="0" max="1"
                value={s.face_match_threshold}
                onChange={(e) => set({ face_match_threshold: num(e.target.value, s.face_match_threshold) })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                How closely a face must match a saved person to count. Higher is stricter and
                gives fewer wrong names.
              </span>
            </label>
            <label className="field" style={{ flex: 1, minWidth: 280 }} title="Plates (or partials) of interest — a match fires a guaranteed high-priority push.">
              plate deny-list (vehicles of interest, comma-separated)
              <input
                type="text"
                placeholder="B8AU77, STOLEN1"
                value={(s.plate_denylist ?? []).join(", ")}
                onChange={(e) =>
                  set({ plate_denylist: e.target.value.split(",").map((x) => x.trim()).filter(Boolean) })
                }
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 280 }} title="Known/expected plates — surfaced as 'known' in review.">
              plate allow-list (known vehicles)
              <input
                type="text"
                placeholder="MYCAR1, SPOUSE2"
                value={(s.plate_allowlist ?? []).join(", ")}
                onChange={(e) =>
                  set({ plate_allowlist: e.target.value.split(",").map((x) => x.trim()).filter(Boolean) })
                }
              />
            </label>
          </div>
        </div>

        <div className="card" data-settings-group="detection">
          <h2>Hand signals</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            The Signals page tracks hand landmarks live in the browser. A held, armed signal logs
            an event and fires any Alarm with a matching <b>gesture</b> condition.
          </p>
          <div className="row">
            <label className="toggle field">
              enable hand signals
              <input
                type="checkbox"
                checked={s.gesture_recognition}
                onChange={() => set({ gesture_recognition: !s.gesture_recognition })}
              />
            </label>
            <label className="field">
              hold time before firing (s)
              <input
                type="number" step="0.1" min="0"
                value={s.gesture_hold_secs}
                onChange={(e) => set({ gesture_hold_secs: num(e.target.value, s.gesture_hold_secs) })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 300 }}>
              armed signals (comma-separated, empty = any)
              <input
                type="text"
                placeholder="open_palm, victory, thumb_up"
                value={(s.gesture_labels ?? []).join(", ")}
                onChange={(e) =>
                  set({
                    gesture_labels: e.target.value
                      .split(",")
                      .map((x) => x.trim())
                      .filter(Boolean),
                  })
                }
              />
            </label>
            <label className="field" title="A silent panic signal: when recognized it always fires at max urgency to your phone-push topic (set under Modes & alerts), even if not in the armed list.">
              duress / help signal
              <select value={s.gesture_duress ?? ""} onChange={(e) => set({ gesture_duress: e.target.value })}>
                <option value="">none</option>
                {["open_palm", "fist", "victory", "point", "thumb_up", "thumb_down", "love", "ok", "call_me"].map(
                  (g) => (
                    <option key={g} value={g}>
                      {prettyGesture(g)}
                    </option>
                  )
                )}
              </select>
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              hand tracking model URL (advanced)
              <input
                type="text"
                value={s.gesture_model_url ?? ""}
                onChange={(e) => set({ gesture_model_url: e.target.value })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Leave blank for the default. Set this only to run fully offline.
              </span>
            </label>
          </div>
        </div>

        <div className="card" data-settings-group="detection">
          <h2>AI event captions (opt-in)</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Generate a short natural-language description of each event for review and search.
            <b> Off by default.</b>
          </p>
          <div className="callout callout-warn" role="note">
            <span className="callout-ico"><IconAlert size={16} /></span>
            <div>
              With the default localhost Ollama URL nothing leaves this machine; pointing it at a
              cloud endpoint <b>sends event snapshots there</b> — a deliberate choice you make here.
            </div>
          </div>
          <div className="row">
            <label className="toggle field">
              enable captions
              <input
                type="checkbox"
                checked={s.genai_enabled}
                onChange={() => set({ genai_enabled: !s.genai_enabled })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              AI server address (Ollama compatible)
              <input
                type="text"
                placeholder="http://localhost:11434/api/generate"
                value={s.genai_url ?? ""}
                onChange={(e) => set({ genai_url: e.target.value })}
              />
            </label>
            <label className="field">
              vision model
              <input
                type="text"
                placeholder="llava"
                value={s.genai_model ?? ""}
                onChange={(e) => set({ genai_model: e.target.value })}
              />
            </label>
            <label className="field" style={{ minWidth: 220 }}>
              API key (cloud only; blank for local)
              <input
                type="password"
                value={s.genai_api_key ?? ""}
                onChange={(e) => set({ genai_api_key: e.target.value })}
              />
            </label>
          </div>
        </div>

        <div className="card" data-settings-group="detection">
          <h2>Speech transcription (what was said)</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Turn spoken words near your cameras into text. When a sound event fires, Cammy
            captures a short clip and writes what it heard onto the event — shown with a{" "}
            <IconMic size={12} /> on Events cards, searchable ("someone yelling help"), and
            usable as a spoken-phrase alarm trigger (Alarms page). Everything runs on this
            machine — audio never leaves it, no extra software to run.
          </p>
          <div className="row">
            <label className="toggle field">
              enable transcription
              <input
                type="checkbox"
                checked={s.transcription_enabled}
                onChange={() => set({ transcription_enabled: !s.transcription_enabled })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              speech model file
              <input
                type="text"
                placeholder="ggml-tiny.en.bin"
                value={s.transcription_model ?? ""}
                onChange={(e) => set({ transcription_model: e.target.value })}
              />
              <small className="muted">
                A Whisper GGML file, downloaded separately — ggml-tiny.en.bin (~75 MB) is a
                good start; ggml-base.en.bin is more accurate.
              </small>
            </label>
          </div>
          <TranscriptionReadiness enabled={s.transcription_enabled} />
        </div>

        <div className="card" data-settings-group="detection">
          <h2>Audio detection</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            The bundled YAMNet model listens for specific sounds — both <b>home-safety</b>{" "}
            (glass break, smoke/fire alarm, gunshot, scream) and <b>family</b> (baby cry,
            child crying, dog bark, cat meow, doorbell) — and raises an audio event you can
            alarm on. Enable it per camera with <b>audio detection</b> on the Cameras page;
            nothing leaves this machine. Pair it with <b>Speech transcription</b> (above) to
            also read what was said.
          </p>
          <label className="field" style={{ maxWidth: 460 }}>
            sensitivity — higher fires fewer, more confident triggers (
            {s.audio_threshold.toFixed(2)})
            <input
              type="range"
              min="0.1"
              max="0.9"
              step="0.05"
              value={s.audio_threshold}
              onChange={(e) => set({ audio_threshold: Number(e.target.value) })}
            />
          </label>
          <div className="muted" style={{ margin: "12px 0 6px" }}>monitored sounds</div>
          <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
            {AUDIO_SOUNDS.map((snd) => {
              const on = snd.values.every((v) => s.audio_labels.includes(v));
              return (
                <TogglePill
                  key={snd.label}
                  on={on}
                  title={snd.values.join(", ")}
                  ariaLabel={`Monitor ${snd.label}`}
                  onClick={() => {
                    const set_ = new Set(s.audio_labels);
                    if (on) snd.values.forEach((v) => set_.delete(v));
                    else snd.values.forEach((v) => set_.add(v));
                    set({ audio_labels: [...set_] });
                  }}
                >
                  {snd.label}
                </TogglePill>
              );
            })}
          </div>
          <small className="muted" style={{ display: "block", marginTop: 8 }}>
            Listening for {s.audio_labels.length} sound type(s).
          </small>
        </div>

        <div className="card" data-settings-group="modes">
          <h2>Modes schedule (auto-arm / disarm)</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Automatically switch the system mode on a schedule — e.g. <b>Away</b> at 08:00 on
            weekdays, <b>Home</b> at 18:00, <b>Disarmed</b> on weekends. The mode gates which
            alarm rules fire, so this also cuts daytime false alerts. Empty = no automation;
            you can still change the mode manually any time.
          </p>
          {(s.arm_schedule ?? []).map((row, i) => (
            <div
              className="row"
              key={i}
              style={{ marginBottom: 6, flexWrap: "wrap", alignItems: "center", gap: 6 }}
            >
              {/* 0 = Sunday, matching the auto-arm worker's day indexing. */}
              {DAY_NAMES.map((d, di) => (
                <TogglePill
                  key={di}
                  on={row.days.includes(di)}
                  ariaLabel={`${d} for schedule row ${i + 1}`}
                  onClick={() =>
                    set({
                      arm_schedule: s.arm_schedule.map((r, j) =>
                        j === i
                          ? {
                              ...r,
                              days: r.days.includes(di)
                                ? r.days.filter((x) => x !== di)
                                : [...r.days, di].sort((a, b) => a - b),
                            }
                          : r
                      ),
                    })
                  }
                >
                  {d}
                </TogglePill>
              ))}
              <input
                type="time"
                value={row.hhmm}
                onChange={(e) =>
                  set({
                    arm_schedule: s.arm_schedule.map((r, j) =>
                      j === i ? { ...r, hhmm: e.target.value } : r
                    ),
                  })
                }
              />
              <select
                value={row.mode}
                onChange={(e) =>
                  set({
                    arm_schedule: s.arm_schedule.map((r, j) =>
                      j === i ? { ...r, mode: e.target.value as ArmMode } : r
                    ),
                  })
                }
              >
                <option value="home">Home</option>
                <option value="away">Away</option>
                <option value="disarmed">Disarmed</option>
              </select>
              <button
                type="button"
                className="danger"
                onClick={() => set({ arm_schedule: s.arm_schedule.filter((_, j) => j !== i) })}
              >
                Remove
              </button>
            </div>
          ))}
          <button
            type="button"
            className="ghost"
            onClick={() =>
              set({ arm_schedule: [...(s.arm_schedule ?? []), { days: [], hhmm: "08:00", mode: "away" }] })
            }
          >
            + Add schedule
          </button>
          <small className="muted" style={{ display: "block", marginTop: 8 }}>
            No days selected = every day. The change applies at the start of the matching minute.
          </small>
        </div>

        <div className="card" data-settings-group="modes">
          <h2>Deterrence actions</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Let alarm rules switch on a real siren, strobe, or light wired to a camera’s alarm
            output. Set it up per rule under Alarms (“trigger siren/light”).
          </p>
          <label className="toggle field" style={{ marginTop: 4 }}>
            allow deterrence actions
            <input
              type="checkbox"
              checked={!!s.deterrence_enabled}
              onChange={() => set({ deterrence_enabled: !s.deterrence_enabled })}
            />
          </label>
          <Callout tone="warn" style={{ marginTop: 10 }}>
            Until this is on, a “trigger siren/light” action does nothing physical — it is a safety
            default. When on, these actions turn on real-world sirens and lights. They only fire while
            the system is armed for that rule’s mode, and never escalate on their own. Test the relay
            from the Alarms page first to confirm it is actually wired.
          </Callout>
        </div>

        <PresenceCard onError={onError} />

        <div className="card" data-settings-group="detection">
          <h2>AI insights</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Opt-in background analysis of your own event history — fully local, nothing leaves this
            machine. Both surface in Notifications and on the Overview page.
          </p>
          <div className="row">
            <label className="toggle field" title="Flag activity that is unusual for a camera at this time of day.">
              anomaly detection
              <input
                type="checkbox"
                checked={!!s.anomaly_detection}
                onChange={() => set({ anomaly_detection: !s.anomaly_detection })}
              />
            </label>
            <label className="toggle field" title="Post a plain-language recap of the day each morning.">
              daily digest
              <input
                type="checkbox"
                checked={!!s.digest_enabled}
                onChange={() => set({ digest_enabled: !s.digest_enabled })}
              />
            </label>
            <button
              type="button"
              className="btn btn-ghost"
              onClick={async () => {
                try {
                  await api.runDigest();
                  toast.success("Digest generated — see the Overview page");
                } catch (e) {
                  onError(String(e));
                }
              }}
            >
              Generate digest now
            </button>
          </div>
        </div>

        <RemoteAccessCard enabled={authEnabled} onEnabled={setAuthEnabled} onError={onError} />

        <PushCard onError={onError} />

        <AccountCard onError={onError} />

        <TwoFactorCard onError={onError} />

        <UsersCard onError={onError} />

        <TokensCard onError={onError} />

        <SharesCard onError={onError} />

        <AuditCard />

        <BackupCard onError={onError} />

        <LicensePane />

        <AboutCard />

        <DesktopCard />

        <div className="card" data-settings-group="recording">
          <h2>Offsite backup</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Mirror recordings to S3-compatible object storage (AWS S3, Backblaze B2, Wasabi,
            Cloudflare R2, or a self-hosted MinIO/NAS) so your footage survives if this machine is
            stolen or its disk fails. Sealed segments upload in the background; the secret key is
            write-only (never sent back; leave blank to keep it). A private/LAN endpoint is fine.
          </p>
          <label className="row" style={{ alignItems: "center", gap: 8 }}>
            <input
              type="checkbox"
              checked={s.offsite_backup_enabled}
              onChange={() => set({ offsite_backup_enabled: !s.offsite_backup_enabled })}
            />
            Enable offsite backup
          </label>
          <label className="row" style={{ alignItems: "center", gap: 8 }}>
            <input
              type="checkbox"
              checked={s.offsite_events_only}
              onChange={() => set({ offsite_events_only: !s.offsite_events_only })}
            />
            Back up only clips around events (saves upload/storage)
          </label>
          <p className="muted" style={{ margin: "0 0 8px 26px", fontSize: 13 }}>
            Only mirror the recordings that cover a detection — not 24/7 footage. Saved (bookmarked)
            clips are always backed up.
          </p>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: 300 }}>
              endpoint URL
              <input
                type="text"
                placeholder="https://s3.us-east-1.amazonaws.com"
                value={s.offsite_endpoint}
                onChange={(e) => set({ offsite_endpoint: e.target.value })}
              />
            </label>
            <label className="field">
              region
              <input
                type="text"
                placeholder="us-east-1"
                value={s.offsite_region}
                onChange={(e) => set({ offsite_region: e.target.value })}
              />
            </label>
          </div>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: 200 }}>
              bucket
              <input
                type="text"
                placeholder="my-camera-backups"
                value={s.offsite_bucket}
                onChange={(e) => set({ offsite_bucket: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 200 }}>
              key prefix (optional)
              <input
                type="text"
                placeholder="cammy"
                value={s.offsite_prefix}
                onChange={(e) => set({ offsite_prefix: e.target.value })}
              />
            </label>
          </div>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: 200 }}>
              access key ID
              <input
                type="text"
                autoComplete="off"
                value={s.offsite_access_key}
                onChange={(e) => set({ offsite_access_key: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 200 }}>
              secret access key
              <input
                type="password"
                autoComplete="new-password"
                placeholder={s.offsite_access_key ? "•••••• (unchanged)" : ""}
                value={s.offsite_secret_key}
                onChange={(e) => set({ offsite_secret_key: e.target.value })}
              />
            </label>
          </div>
          <OffsiteStatusReadout />
        </div>

        <div className="card" data-settings-group="modes">
          <h2>Email (SMTP)</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Send alarm emails with the snapshot attached — add an <b>email</b> action to any Alarm
            rule. Use <code>smtps://host:465</code> for implicit TLS or <code>smtp://host:587</code>{" "}
            for STARTTLS. The password is write-only (never sent back; leave blank to keep it).
          </p>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: 280 }}>
              SMTP server URL
              <input
                type="text"
                placeholder="smtps://smtp.example.com:465"
                value={s.smtp_url}
                onChange={(e) => set({ smtp_url: e.target.value })}
              />
            </label>
            <label className="field">
              username
              <input
                type="text"
                autoComplete="off"
                value={s.smtp_user}
                onChange={(e) => set({ smtp_user: e.target.value })}
              />
            </label>
            <label className="field">
              password
              <input
                type="password"
                autoComplete="new-password"
                placeholder={s.smtp_url ? "•••••• (unchanged)" : ""}
                value={s.smtp_pass}
                onChange={(e) => set({ smtp_pass: e.target.value })}
              />
            </label>
          </div>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: 240 }}>
              from address
              <input
                type="email"
                inputMode="email"
                autoComplete="email"
                placeholder="nvr@example.com"
                value={s.smtp_from}
                onChange={(e) => set({ smtp_from: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 240 }}>
              default recipient(s), comma-separated
              <input
                type="text"
                inputMode="email"
                placeholder="me@example.com"
                value={s.smtp_to}
                onChange={(e) => set({ smtp_to: e.target.value })}
              />
            </label>
          </div>
        </div>

        <div className="card" data-settings-group="security">
          <h2>Reverse-proxy SSO</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Skip the Cammy login when you already sign in through a gateway like Cloudflare
            Access or Authelia. Cammy sits behind the auth proxy (Authelia, oauth2-proxy,
            Cloudflare Access, Tailscale) and trusts the authenticated-user header it sets.
            Enter the user header name your proxy sends below. Leave the user header blank to
            turn SSO off. If the username matches a Cammy account, that account's role applies.
          </p>
          <div className="callout callout-danger" role="note">
            <span className="callout-ico"><IconAlert size={16} /></span>
            <div>
              Only enable this when the server is started with <code>--trusted-proxy</code>, is
              reachable <i>only</i> through that proxy, and the proxy <i>sets and strips</i> these
              headers so a client can't inject them — otherwise a spoofed header is a remote auth
              bypass. (Cammy already ignores the header on any request that didn't arrive through the
              proxy — i.e. without <code>X-Forwarded-For</code>.)
            </div>
          </div>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: 220 }}>
              user header (blank = off)
              <input
                type="text"
                placeholder="Remote-User"
                value={s.auth_proxy_header}
                onChange={(e) => set({ auth_proxy_header: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 220 }}>
              role/group header (optional)
              <input
                type="text"
                placeholder="Remote-Groups"
                value={s.auth_proxy_role_header}
                onChange={(e) => set({ auth_proxy_role_header: e.target.value })}
              />
            </label>
            <label className="field" title="Role for an SSO user with no role header and no matching Cammy account.">
              default role
              <select
                value={s.auth_proxy_default_role}
                onChange={(e) => set({ auth_proxy_default_role: e.target.value })}
              >
                <option value="viewer">viewer</option>
                <option value="operator">operator</option>
                <option value="admin">admin</option>
              </select>
            </label>
          </div>
        </div>

        <div className="card" data-settings-group="modes">
          <h2>Notifications</h2>
          <div className="row">
            <label className="field" style={{ minWidth: 260 }}>
              notify me only about
              <select
                value={s.notify_min_severity ?? 1}
                onChange={(e) => set({ notify_min_severity: Number(e.target.value) })}
              >
                <option value={1}>everything (default)</option>
                <option value={2}>normal and up (skip routine wildlife)</option>
                <option value={3}>high & critical only</option>
                <option value={4}>critical only</option>
              </select>
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Quiets phone push & email only — webhooks, MQTT and duress alerts always deliver.
              </span>
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              webhook URL (POST per event; empty = off)
              <input
                type="text"
                placeholder="http://homeassistant.local:8123/api/webhook/cammy"
                value={s.webhook_url}
                onChange={(e) => set({ webhook_url: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              camera health push — ntfy topic URL (offline/online alerts; empty = off)
              <input
                type="text"
                placeholder="https://ntfy.sh/your-secret-topic"
                value={s.health_ntfy_url ?? ""}
                onChange={(e) => set({ health_ntfy_url: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              public base URL (adds tap-through clip/snapshot links to pushes)
              <input
                type="text"
                placeholder="https://nvr.example.com"
                value={s.public_base_url ?? ""}
                onChange={(e) => set({ public_base_url: e.target.value })}
              />
              {baseUrlWarning(s.public_base_url ?? "") && (
                <span
                  className="muted"
                  style={{ color: "var(--warn)", fontSize: "var(--text-sm)", marginTop: 4, display: "inline-flex", alignItems: "center", gap: 5 }}
                >
                  <IconAlert size={13} /> {baseUrlWarning(s.public_base_url ?? "")}
                </span>
              )}
            </label>
            <label className="field" style={{ minWidth: 240 }}>
              MQTT broker (empty = off)
              <input
                type="text"
                placeholder="mqtt://homeassistant.local:1883"
                value={s.mqtt_url}
                onChange={(e) => set({ mqtt_url: e.target.value })}
              />
            </label>
            <label className="field">
              MQTT topic prefix
              <input
                type="text"
                value={s.mqtt_prefix}
                onChange={(e) => set({ mqtt_prefix: e.target.value })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Text at the start of every topic. The default is fine.
              </span>
            </label>
            <label className="toggle field" title="Publish MQTT-discovery configs so Home Assistant auto-creates a binary_sensor per (camera, object) and a last-detection sensor per camera.">
              Home Assistant discovery
              <input
                type="checkbox"
                checked={s.mqtt_ha_discovery}
                onChange={() => set({ mqtt_ha_discovery: !s.mqtt_ha_discovery })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Lets Home Assistant find your cameras automatically.
              </span>
            </label>
            <label className="field">
              HA discovery prefix
              <input
                type="text"
                value={s.mqtt_ha_prefix}
                onChange={(e) => set({ mqtt_ha_prefix: e.target.value })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Text at the start of every topic. The default is fine.
              </span>
            </label>
            <label className="field" title="Seconds a Home Assistant binary_sensor stays ON after a detection before auto-clearing.">
              sensor ON timeout (s)
              <input
                type="number" min="1"
                value={s.mqtt_state_timeout_secs}
                onChange={(e) => set({ mqtt_state_timeout_secs: num(e.target.value, s.mqtt_state_timeout_secs) })}
              />
            </label>
          </div>
          <div className="row" style={{ marginTop: 10 }}>
            <label className="toggle field">
              Accept commands over MQTT (arm / trigger)
              <input
                type="checkbox"
                checked={s.mqtt_commands_enabled ?? false}
                onChange={() => set({ mqtt_commands_enabled: !s.mqtt_commands_enabled })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Lets Home Assistant (or any broker client) arm/disarm and trigger cameras by
                publishing to <code>{(s.mqtt_prefix || "zoomy")}/cmd/arm</code> (payload{" "}
                <code>home</code>/<code>away</code>/<code>disarmed</code>) and{" "}
                <code>{(s.mqtt_prefix || "zoomy")}/cmd/trigger</code> (payload = camera id or name).
              </span>
            </label>
          </div>
          {s.mqtt_commands_enabled && (
            <div className="callout callout-warn" role="note" style={{ marginTop: 8 }}>
              <span className="callout-ico"><IconAlert size={16} /></span>
              <div>
                This is a control surface: <b>anyone who can publish to your MQTT broker can
                arm/disarm and trigger cameras.</b> Only enable it on a broker you trust and control.
                Every accepted command is written to the security audit log.
              </div>
            </div>
          )}
          <p className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 8 }}>
            Home Assistant can also read a live event feed at{" "}
            <code>GET /api/events/stream</code> (Server-Sent Events, Bearer-token auth). See the
            Cammy Home Assistant custom component under <code>integrations/homeassistant/cammy</code>.
          </p>
          <div className="row" style={{ marginTop: 10 }}>
            <label className="field" style={{ flex: 1, minWidth: 420 }}>
              webhook body template (empty = default JSON)
              <textarea
                rows={2}
                placeholder='{"text":"{{label}} on {{camera}} ({{score}})"}'
                value={s.webhook_template ?? ""}
                onChange={(e) => set({ webhook_template: e.target.value })}
                style={{ width: "100%", fontFamily: "monospace" }}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Placeholders: {"{{camera}} {{label}} {{score}} {{snapshot}} {{face}} {{plate}} {{transcript}} {{caption}} {{severity}}"} — each is replaced with the event's value when the webhook fires.
              </span>
            </label>
          </div>
        </div>

        <div className="card" data-settings-group="recording">
          <h2>Recording &amp; retention</h2>
          <div className="row">
            <label className="field">
              segment length (s)
              <input
                type="number" min="10"
                value={s.segment_seconds}
                onChange={(e) => set({ segment_seconds: num(e.target.value, s.segment_seconds) })}
              />
            </label>
            <label className="field">
              keep at most (days)
              <input
                type="number" min="1"
                value={s.retention_days}
                onChange={(e) => set({ retention_days: num(e.target.value, s.retention_days) })}
              />
            </label>
            <label className="field">
              keep at most (GB)
              <input
                type="number" min="1"
                value={s.retention_gb}
                onChange={(e) => set({ retention_gb: num(e.target.value, s.retention_gb) })}
              />
            </label>
            <label className="field">
              reduce quality after (days, 0 = off)
              <input
                type="number" min="0"
                value={s.enhanced_retention_days}
                onChange={(e) =>
                  set({ enhanced_retention_days: num(e.target.value, s.enhanced_retention_days) })
                }
              />
            </label>
            <label className="field" title="Hardware video encoder for the enhanced-retention re-encode. Falls back to CPU automatically if unavailable.">
              re-encode with
              <select value={s.hwaccel ?? ""} onChange={(e) => set({ hwaccel: e.target.value })}>
                <option value="">CPU (libx264)</option>
                <option value="nvenc">NVIDIA NVENC</option>
                <option value="qsv">Intel QuickSync</option>
                <option value="videotoolbox">Apple VideoToolbox</option>
              </select>
            </label>
            <label className="field">
              keep events (days)
              <input
                type="number" min="1"
                value={s.event_retention_days}
                onChange={(e) =>
                  set({ event_retention_days: num(e.target.value, s.event_retention_days) })
                }
              />
            </label>
            <label className="field" style={{ minWidth: 300 }}>
              recordings folder (empty = data/recordings; another drive or NAS share works)
              <input
                type="text"
                placeholder="D:\cammy-recordings or \\nas\cams"
                value={s.recordings_dir ?? ""}
                onChange={(e) => set({ recordings_dir: e.target.value })}
              />
            </label>
            <label className="field">
              detector model file
              <input
                type="text"
                value={s.model_path}
                onChange={(e) => set({ model_path: e.target.value })}
              />
              <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                Path to the object detection model. Leave as is unless you know you need to
                change it.
              </span>
            </label>
            <label
              className="field"
              title="YOLOv8-pose model for the server-side body-pose worker (fall / crib standing / covered-face). Download yolov8n-pose.onnx and put it beside the detector model; the worker idles until it exists and a camera turns on 'body pose monitoring'."
            >
              pose model path (body pose monitoring)
              <input
                type="text"
                value={s.pose_model ?? ""}
                placeholder="yolov8n-pose.onnx"
                onChange={(e) => set({ pose_model: e.target.value })}
              />
            </label>
            <label className="toggle field" title="Audio is saved in the recording as AAC.">
              record audio
              <input
                type="checkbox"
                checked={s.record_audio}
                onChange={() => set({ record_audio: !s.record_audio })}
              />
            </label>
          </div>
        </div>

        <div className="row save-bar">
          <button className="btn btn-primary">Save</button>
          {dirty ? (
            <span className="badge accent">Unsaved changes</span>
          ) : saved ? (
            <span className="save-ok"><IconCheck size={15} /> Saved</span>
          ) : null}
          <span className="muted">Changes apply within a few seconds — no restart needed.</span>
        </div>
      </form>
    </div>
  );
}
