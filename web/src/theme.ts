// Theme (dark default, optional light). The token system in styles.css exposes a
// full [data-theme="light"] override, so switching is a single attribute flip.
// Persisted in localStorage; first-run falls back to the OS preference. The actual
// first-paint application happens via the inline script in index.html (no flash);
// this module keeps it in sync at runtime.

export type Theme = "dark" | "light";

const KEY = "zoomy-theme";

export function getTheme(): Theme {
  const stored = localStorage.getItem(KEY);
  if (stored === "light" || stored === "dark") return stored;
  return window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

export function applyTheme(t: Theme) {
  if (t === "light") document.documentElement.dataset.theme = "light";
  else delete document.documentElement.dataset.theme;
  // Keep the mobile browser chrome in step with the surface color.
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute("content", t === "light" ? "#f7f8fa" : "#0a0b0e");
}

export function setTheme(t: Theme) {
  localStorage.setItem(KEY, t);
  applyTheme(t);
}

export function toggleTheme(): Theme {
  const next: Theme = getTheme() === "light" ? "dark" : "light";
  setTheme(next);
  return next;
}
