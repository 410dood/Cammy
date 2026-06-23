import { ChangeEvent, FormEvent, useEffect, useState } from "react";
import { api, ApiToken, ArmMode, AuditEntry, fmtTime, Me, Role, Settings as S, User } from "../api";
import { useToast, useDialog, RelTime } from "../ui";
import {
  IconProps, IconLogIn, IconBan, IconKey, IconLock, IconTicket, IconTrash,
  IconDownload, IconUpload, IconCheck, IconUser,
} from "../icons";

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

// Day-of-week labels for the auto-arm schedule (0 = Sunday, matches the worker).
const DOW = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

function AuditCard() {
  const [rows, setRows] = useState<AuditEntry[]>([]);
  useEffect(() => {
    api.audit(100).then(setRows).catch(() => {});
  }, []);
  if (rows.length === 0) return null;
  return (
    <div className="card">
      <h2>Recent security activity</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Logins, password changes, and API-token changes — most recent first. Useful for
        spotting unexpected access on a WAN-exposed server.
      </p>
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
  );
}

function RemoteAccessCard({ onError }: { onError: (e: string) => void }) {
  const [enabled, setEnabled] = useState(false);
  const [pw, setPw] = useState("");
  const [msg, setMsg] = useState("");

  useEffect(() => {
    api.authStatus().then((a) => setEnabled(a.enabled)).catch(() => {});
  }, []);

  const apply = async (password: string) => {
    try {
      const r = await api.setPassword(password);
      setEnabled(r.enabled);
      setPw("");
      setMsg(r.enabled ? "Password set — other devices must now log in." : "Password cleared.");
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <div className="card">
      <h2>Remote access</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        When a password is set, other devices on your network must log in. This computer
        (localhost / the desktop app) is always exempt.
      </p>
      <div className="row">
        <span className={`pill ${enabled ? "on" : ""}`}>{enabled ? "protected" : "open"}</span>
        <input
          type="password"
          placeholder="new password (min 6 chars)"
          value={pw}
          onChange={(e) => setPw(e.target.value)}
        />
        <button type="button" className="primary" disabled={pw.trim().length < 6} onClick={() => apply(pw)}>
          Set password
        </button>
        {enabled && (
          <button type="button" className="danger" onClick={() => apply("")}>
            Clear
          </button>
        )}
        {msg && <span style={{ color: "var(--ok)" }}>{msg}</span>}
      </div>
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

  const load = () => {
    api.tokens().then(setTokens).catch(() => {});
  };
  useEffect(load, []);

  const create = async () => {
    const n = name.trim();
    if (!n) return;
    try {
      const r = await api.createToken(n, role);
      setFresh({ name: r.name, role: r.role, token: r.token });
      setName("");
      load();
    } catch (e) {
      onError(String(e));
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
    <div className="card">
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
        <button type="button" className="primary" disabled={!name.trim()} onClick={create}>
          Create token
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
          <code style={{ userSelect: "all", wordBreak: "break-all" }}>{fresh.token}</code>
        </div>
      )}
      {tokens.length > 0 && (
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

  useEffect(() => {
    api.me().then(setMe).catch(() => {});
  }, []);

  if (!me || !me.named) return null;

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
    <div className="card">
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
    try {
      const backup = JSON.parse(await file.text());
      const r = await api.restore(backup);
      toast.success("Configuration restored — reload to see changes");
      setMsg(
        `Restored — ${r.cameras_added} camera(s) added, ${r.cameras_skipped} skipped, ${r.alarms_added} alarm(s) added, settings applied. Reload to see changes.`,
      );
    } catch (err) {
      onError(`restore failed: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card">
      <h2>Backup &amp; restore</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Export your configuration — cameras, settings and alarm rules — to a JSON file to move to
        another machine. Recordings, events and enrolled faces are <b>not</b> included. The file can
        contain camera credentials, so keep it private. Restore is additive: settings are replaced,
        but a camera/alarm whose name already exists is left untouched.
      </p>
      <div className="row" style={{ alignItems: "center" }}>
        <a className="btn btn-ghost" href="/api/backup" download="zoomy-backup.json">
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

function UsersCard({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const dialog = useDialog();
  const [me, setMe] = useState<Me | null>(null);
  const [users, setUsers] = useState<User[]>([]);
  const [name, setName] = useState("");
  const [pw, setPw] = useState("");
  const [role, setRole] = useState<Role>("viewer");

  const load = () => {
    api.me().then(setMe).catch(() => {});
    api.users().then(setUsers).catch(() => {});
  };
  useEffect(load, []);

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
  const remove = async (u: User) => {
    if (!(await dialog.confirm({ title: `Delete ${u.username}?`, confirmLabel: "Delete", danger: true })))
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
    <div className="card">
      <h2>Users &amp; roles</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Named accounts, each with a role: <b>admin</b> (full control, incl. users),{" "}
        <b>operator</b> (manage cameras, settings, alarms), <b>viewer</b> (read-only + live).
        This computer (localhost) and the legacy single password always have admin, so you can
        never lock yourself out locally.
      </p>
      <div className="row">
        <input type="text" placeholder="username" value={name} onChange={(e) => setName(e.target.value)} />
        <input
          type="password"
          placeholder="password (min 6)"
          value={pw}
          onChange={(e) => setPw(e.target.value)}
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
                <td style={{ textAlign: "right", whiteSpace: "nowrap" }}>
                  <button type="button" className="btn btn-ghost ev-act" onClick={() => resetPw(u)}>
                    Reset password
                  </button>
                  <button type="button" className="btn btn-danger ev-act" onClick={() => remove(u)}>
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

// Sticky in-page section nav for the long Settings page. Self-scanning: it reads
// each rendered `.card`'s <h2>, gives the card a slug id, and renders jump chips
// with scroll-spy — so new cards appear in the nav automatically, no wiring.
function SettingsNav() {
  const [sections, setSections] = useState<{ id: string; label: string }[]>([]);
  const [active, setActive] = useState("");

  useEffect(() => {
    const scan = () => {
      const root = document.querySelector(".settings-page");
      if (!root) return;
      const secs = [...root.querySelectorAll<HTMLElement>(".card")]
        .map((c) => {
          const label = c.querySelector("h2")?.textContent?.trim();
          if (!label) return null;
          const id = "set-" + label.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/(^-|-$)/g, "");
          c.id = id;
          return { id, label };
        })
        .filter((x): x is { id: string; label: string } => x !== null);
      setSections(secs);
    };
    scan();
    // Re-scan once for cards that appear after an async load (Users, AI insights).
    const t = setTimeout(scan, 700);
    return () => clearTimeout(t);
  }, []);

  useEffect(() => {
    if (!sections.length) return;
    const obs = new IntersectionObserver(
      (entries) => {
        for (const e of entries) if (e.isIntersecting) setActive((e.target as HTMLElement).id);
      },
      { rootMargin: "-72px 0px -70% 0px" },
    );
    for (const s of sections) {
      const el = document.getElementById(s.id);
      if (el) obs.observe(el);
    }
    return () => obs.disconnect();
  }, [sections]);

  if (sections.length < 3) return null;

  return (
    <nav className="settings-nav" aria-label="Settings sections">
      {sections.map((s) => (
        <button
          key={s.id}
          type="button"
          className={`settings-nav-chip ${active === s.id ? "active" : ""}`}
          onClick={() =>
            document.getElementById(s.id)?.scrollIntoView({ behavior: "smooth", block: "start" })
          }
        >
          {s.label}
        </button>
      ))}
    </nav>
  );
}

export default function Settings({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const [s, setS] = useState<S | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    api.settings().then(setS).catch((e) => onError(String(e)));
  }, [onError]);

  if (!s) return <p className="muted">Loading…</p>;

  const set = (patch: Partial<S>) => {
    setS({ ...s, ...patch });
    setSaved(false);
  };

  const save = async (e: FormEvent) => {
    e.preventDefault();
    try {
      setS(await api.saveSettings(s));
      setSaved(true);
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
      <SettingsNav />
      <form onSubmit={save}>
        <div className="card">
          <h2>Detection</h2>
          <div className="row">
            <label className="field">
              objects (comma-separated, empty = all)
              <input
                type="text"
                style={{ minWidth: 380 }}
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
            </label>
            <label className="field">
              motion threshold (0-1)
              <input
                type="number" step="0.005" min="0" max="1"
                value={s.motion_threshold}
                onChange={(e) => set({ motion_threshold: num(e.target.value, s.motion_threshold) })}
              />
            </label>
            <label className="field">
              sample interval (ms)
              <input
                type="number" step="100" min="100"
                value={s.poll_ms}
                onChange={(e) => set({ poll_ms: num(e.target.value, s.poll_ms) })}
              />
            </label>
            <label className="field">
              event cooldown (s)
              <input
                type="number" min="0"
                value={s.event_cooldown_secs}
                onChange={(e) => set({ event_cooldown_secs: num(e.target.value, s.event_cooldown_secs) })}
              />
            </label>
            <label className="toggle field">
              force CPU
              <input type="checkbox" checked={s.force_cpu} onChange={() => set({ force_cpu: !s.force_cpu })} />
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

        <div className="card">
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
            <label className="field" title="A silent panic signal: when recognized it always fires at max push urgency (and pushes to the health ntfy topic), even if not in the armed list.">
              duress / help signal
              <select value={s.gesture_duress ?? ""} onChange={(e) => set({ gesture_duress: e.target.value })}>
                <option value="">none</option>
                {["open_palm", "fist", "victory", "point", "thumb_up", "thumb_down", "love", "ok", "call_me"].map(
                  (g) => (
                    <option key={g} value={g}>
                      {g}
                    </option>
                  )
                )}
              </select>
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              model URL (MediaPipe .task; default = Google CDN, override to self-host offline)
              <input
                type="text"
                value={s.gesture_model_url ?? ""}
                onChange={(e) => set({ gesture_model_url: e.target.value })}
              />
            </label>
          </div>
        </div>

        <div className="card">
          <h2>AI event captions (opt-in)</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Generate a short natural-language description of each event for review and search.
            <b> Off by default.</b> With the default localhost Ollama URL nothing leaves this
            machine; pointing it at a cloud endpoint sends snapshots there — that's a deliberate
            choice you make here.
          </p>
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
              endpoint (Ollama-compatible /api/generate)
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

        <div className="card">
          <h2>Audio transcription</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Speech-to-text for audio events, using a <b>bundled, in-process</b> whisper.cpp engine —
            audio never leaves this machine and there's no separate software to run.{" "}
            <b>Off by default.</b> When on, this applies to <b>every camera with audio detection
            enabled</b>: a sound event captures a short clip and the transcript is written onto the
            event (shown on cards). Needs the whisper model file present.
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
            <label className="field" style={{ flex: 1, minWidth: 320 }} title="Path to a whisper GGML model (downloaded separately), e.g. ggml-tiny.en.bin or ggml-base.en.bin.">
              whisper model file
              <input
                type="text"
                placeholder="ggml-tiny.en.bin"
                value={s.transcription_model ?? ""}
                onChange={(e) => set({ transcription_model: e.target.value })}
              />
            </label>
          </div>
        </div>

        <div className="card">
          <h2>Audio detection</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            The bundled YAMNet model listens for specific sounds — both <b>home-safety</b>{" "}
            (glass break, smoke/fire alarm, gunshot, scream) and <b>family</b> (baby cry,
            child crying, dog bark, cat meow, doorbell) — and raises an audio event you can
            alarm on. Enable it per camera with <b>audio detection</b> on the Cameras page;
            nothing leaves this machine.
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
                <span
                  key={snd.label}
                  className={`pill toggle ${on ? "on" : ""}`}
                  title={snd.values.join(", ")}
                  onClick={() => {
                    const set_ = new Set(s.audio_labels);
                    if (on) snd.values.forEach((v) => set_.delete(v));
                    else snd.values.forEach((v) => set_.add(v));
                    set({ audio_labels: [...set_] });
                  }}
                >
                  {snd.label}
                </span>
              );
            })}
          </div>
          <small className="muted" style={{ display: "block", marginTop: 8 }}>
            {s.audio_labels.length} AudioSet label(s) active. Chips map to exact YAMNet
            class names so detection fires reliably.
          </small>
        </div>

        <div className="card">
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
              {DOW.map((d, di) => (
                <span
                  key={di}
                  className={`pill toggle ${row.days.includes(di) ? "on" : ""}`}
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
                </span>
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
                remove
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
            + add schedule
          </button>
          <small className="muted" style={{ display: "block", marginTop: 8 }}>
            No days selected = every day. The change applies at the start of the matching minute.
          </small>
        </div>

        <div className="card">
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

        <RemoteAccessCard onError={onError} />

        <AccountCard onError={onError} />

        <UsersCard onError={onError} />

        <TokensCard onError={onError} />

        <AuditCard />

        <BackupCard onError={onError} />

        <div className="card">
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
                type="text"
                placeholder="nvr@example.com"
                value={s.smtp_from}
                onChange={(e) => set({ smtp_from: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 240 }}>
              default recipient(s), comma-separated
              <input
                type="text"
                placeholder="me@example.com"
                value={s.smtp_to}
                onChange={(e) => set({ smtp_to: e.target.value })}
              />
            </label>
          </div>
        </div>

        <div className="card">
          <h2>Notifications</h2>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              webhook URL (POST per event; empty = off)
              <input
                type="text"
                placeholder="http://homeassistant.local:8123/api/webhook/zoomy"
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
            </label>
            <label className="toggle field" title="Publish MQTT-discovery configs so Home Assistant auto-creates a binary_sensor per (camera, object) and a last-detection sensor per camera.">
              Home Assistant discovery
              <input
                type="checkbox"
                checked={s.mqtt_ha_discovery}
                onChange={() => set({ mqtt_ha_discovery: !s.mqtt_ha_discovery })}
              />
            </label>
            <label className="field">
              HA discovery prefix
              <input
                type="text"
                value={s.mqtt_ha_prefix}
                onChange={(e) => set({ mqtt_ha_prefix: e.target.value })}
              />
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
            <label className="field" style={{ flex: 1, minWidth: 420 }}>
              webhook body template (empty = default JSON; placeholders like{" "}
              <code>{"{{camera}}"}</code> <code>{"{{label}}"}</code> <code>{"{{score}}"}</code>{" "}
              <code>{"{{snapshot}}"}</code> — see docs/03)
              <textarea
                rows={2}
                placeholder='{"text":"{{label}} on {{camera}} ({{score}})"}'
                value={s.webhook_template ?? ""}
                onChange={(e) => set({ webhook_template: e.target.value })}
                style={{ width: "100%", fontFamily: "monospace" }}
              />
            </label>
          </div>
        </div>

        <div className="card">
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
                placeholder="D:\zoomy-recordings or \\nas\cams"
                value={s.recordings_dir ?? ""}
                onChange={(e) => set({ recordings_dir: e.target.value })}
              />
            </label>
            <label className="field">
              model path
              <input
                type="text"
                value={s.model_path}
                onChange={(e) => set({ model_path: e.target.value })}
              />
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
            <label className="toggle field">
              record audio (AAC)
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
          {saved && (
            <span className="save-ok"><IconCheck size={15} /> Saved</span>
          )}
          <span className="muted">Changes apply within a few seconds — no restart needed.</span>
        </div>
      </form>
    </div>
  );
}
