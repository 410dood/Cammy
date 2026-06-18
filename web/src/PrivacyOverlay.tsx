// B4 — privacy redaction: render a camera's configured privacy masks as BLURRED
// regions over the live video (blur, not blackout, so you keep situational
// awareness). The masks already exist per-camera (set in the zone editor) and
// previously only excluded those areas from AI detection; the live stream showed
// them in full. This obscures them on screen too. Pure client-side overlay.

export default function PrivacyOverlay({ masks }: { masks: [number, number][][] }) {
  const polys = (masks ?? []).filter((p) => p && p.length >= 3);
  if (polys.length === 0) return null;
  return (
    <div className="privacy-overlay" aria-hidden="true">
      {polys.map((poly, i) => {
        const points = poly
          .map(([x, y]) => `${(x * 100).toFixed(2)}% ${(y * 100).toFixed(2)}%`)
          .join(", ");
        return <div key={i} className="privacy-region" style={{ clipPath: `polygon(${points})` }} />;
      })}
    </div>
  );
}
