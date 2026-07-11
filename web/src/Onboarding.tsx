// C3 — first-run onboarding wizard. Appears once when no cameras are configured:
// welcome, optionally secure the box, then point the user at adding a camera.
// Self-contained over existing endpoints; the real add lives on the Cameras page.

import { useEffect, useRef, useState } from "react";
import { api, DiscoveredCam } from "./api";
import { useToast, useFocusTrap } from "./ui";
import { IconShield, IconRadar, IconVideo, IconCheck, IconLock } from "./icons";

const SEEN_KEY = "zoomy-onboarded";

export function markOnboarded() {
  localStorage.setItem(SEEN_KEY, "1");
}
export function shouldOnboard(): boolean {
  return localStorage.getItem(SEEN_KEY) !== "1";
}

export default function Onboarding({
  onAddCamera,
  onClose,
}: {
  onAddCamera: () => void;
  onClose: () => void;
}) {
  const toast = useToast();
  const [step, setStep] = useState(0);
  const [pw, setPw] = useState("");
  const [saving, setSaving] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [found, setFound] = useState<DiscoveredCam[] | null>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  useFocusTrap(cardRef);
  useEffect(() => {
    cardRef.current?.focus();
  }, []);

  const finish = () => {
    markOnboarded();
    onClose();
  };

  const setPassword = async () => {
    if (saving) return;
    setSaving(true);
    try {
      await api.setPassword(pw);
      toast.success("Password set — other devices will need it");
      setStep(2);
    } catch (e) {
      toast.error(String(e));
    } finally {
      setSaving(false);
    }
  };

  const scan = async () => {
    setScanning(true);
    try {
      const r = await api.scanNetwork();
      setFound(r.cameras);
    } catch {
      setFound([]);
    } finally {
      setScanning(false);
    }
  };

  return (
    <div className="modal-bg">
      <div ref={cardRef} tabIndex={-1} className="onb" role="dialog" aria-modal="true" aria-label="Welcome to Cammy">
        <div className="onb-steps">
          {["Welcome", "Security", "Cameras"].map((s, i) => (
            <span key={s} className={`onb-step ${i === step ? "active" : ""} ${i < step ? "done" : ""}`}>
              <span className="onb-dot">{i < step ? <IconCheck size={12} /> : i + 1}</span>
              {s}
            </span>
          ))}
        </div>

        {step === 0 && (
          <div className="onb-body">
            <span className="onb-hero"><IconVideo size={26} /></span>
            <h2>Welcome to Cammy</h2>
            <p className="muted">
              Your self-hosted, private NVR with on-device AI. Nothing leaves this machine. Let's
              get your first camera streaming in a minute.
            </p>
            <p className="muted" style={{ marginTop: 8 }}>
              <b>Every feature is unlocked free for 30 days</b> — no card, no account. And it never
              stops recording when the trial ends.
            </p>
            <div className="onb-actions">
              <button className="btn btn-ghost" onClick={finish}>Skip setup</button>
              <button className="btn btn-primary" onClick={() => setStep(1)}>Get started</button>
            </div>
          </div>
        )}

        {step === 1 && (
          <div className="onb-body">
            <span className="onb-hero"><IconShield size={26} /></span>
            <h2>Secure remote access</h2>
            <p className="muted">
              Set a password so other devices on your network must log in. This computer is always
              exempt. You can skip and do this later in Settings.
            </p>
            <div className="row" style={{ width: "100%" }}>
              <span className="onb-ico"><IconLock size={16} /></span>
              <input
                type="password"
                autoComplete="new-password"
                placeholder="new password (min 6 chars)"
                value={pw}
                onChange={(e) => setPw(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && pw.trim().length >= 6 && setPassword()}
                style={{ flex: 1 }}
              />
            </div>
            <div className="onb-actions">
              <button className="btn btn-ghost" onClick={() => setStep(2)}>Skip</button>
              <button className="btn btn-primary" disabled={saving || pw.trim().length < 6} onClick={setPassword}>
                {saving ? "Setting…" : "Set password"}
              </button>
            </div>
          </div>
        )}

        {step === 2 && (
          <div className="onb-body">
            <span className="onb-hero"><IconRadar size={26} /></span>
            <h2>Add your first camera</h2>
            <p className="muted">
              Scan your network for cameras, or add one by typing its IP address. You'll finish
              adding it on the Cameras page, where you can enter credentials and pick what to record.
            </p>
            <p className="muted" style={{ fontSize: "var(--text-sm)" }}>
              Once a camera connects, <b>recording starts right away</b> (see Recordings). AI events
              appear under Events when a camera sees a person or vehicle move.
            </p>
            <button className="btn btn-secondary" disabled={scanning} onClick={scan}>
              <IconRadar size={15} /> {scanning ? "Scanning…" : "Scan network"}
            </button>
            {found && (
              <p className="muted" style={{ fontSize: "var(--text-sm)" }}>
                {found.length === 0
                  ? "No cameras answered the scan. You can still add one by its network address on the Cameras page."
                  : `Found ${found.length} camera${found.length === 1 ? "" : "s"}: ${found.map((c) => c.host).join(", ")}`}
              </p>
            )}
            <div className="onb-actions">
              <button className="btn btn-ghost" onClick={finish}>I'll do it later</button>
              <button
                className="btn btn-primary"
                onClick={() => {
                  markOnboarded();
                  onAddCamera();
                }}
              >
                Add a camera
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
