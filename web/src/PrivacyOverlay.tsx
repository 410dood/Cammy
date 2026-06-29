// B4 — privacy redaction: render a camera's configured privacy masks as BLURRED
// regions over the live video (blur, not blackout, so you keep situational
// awareness). The masks already exist per-camera (set in the zone editor) and
// previously only excluded those areas from AI detection; the live stream showed
// them in full. This obscures them on screen too. Pure client-side overlay.

import { IconShield } from "./icons";

export default function PrivacyOverlay({ masks }: { masks: [number, number][][] }) {
  const polys = (masks ?? []).filter((p) => p && p.length >= 3);
  if (polys.length === 0) return null;
  return (
    <>
      <div className="privacy-overlay" aria-hidden="true">
        {polys.map((poly, i) => {
          const points = poly
            .map(([x, y]) => `${(x * 100).toFixed(2)}% ${(y * 100).toFixed(2)}%`)
            .join(", ");
          return <div key={i} className="privacy-region" style={{ clipPath: `polygon(${points})` }} />;
        })}
      </div>
      {/* A visible tag so a deliberate redaction reads as intentional, not a
          broken/defocused lens. */}
      <span className="privacy-tag" title="This camera has privacy masks applied">
        <IconShield size={12} /> Privacy
      </span>
    </>
  );
}
