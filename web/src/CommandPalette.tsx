// Command palette (Cmd/Ctrl-K): fuzzy jump to a page, a camera, or an action.
// Front door for power users; no router, no dependency. Keyboard-first:
// ↑/↓ move, Enter runs, Esc closes, type to filter.

import { ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { IconSearch } from "./icons";
import { useFocusTrap } from "./ui";

export interface Command {
  id: string;
  label: string;
  hint?: string;
  group?: string;
  icon?: ReactNode;
  keywords?: string;
  run: () => void;
}

/** Lowercased token-subsequence match + light ranking (lower score = better). */
function rank(cmd: Command, tokens: string[]): number | null {
  const label = cmd.label.toLowerCase();
  const hay = `${label} ${(cmd.keywords ?? "").toLowerCase()} ${(cmd.group ?? "").toLowerCase()}`;
  let score = 0;
  for (const t of tokens) {
    const inLabel = label.indexOf(t);
    if (inLabel >= 0) {
      score += inLabel; // earlier in the label ranks higher
    } else if (hay.includes(t)) {
      score += 50; // matched on keywords/group, not the visible label
    } else {
      return null; // every token must match somewhere
    }
  }
  return score + label.length * 0.01; // tie-break toward shorter labels
}

export default function CommandPalette({
  commands,
  onClose,
}: {
  commands: Command[];
  onClose: () => void;
}) {
  const [q, setQ] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  useFocusTrap(cardRef);

  const results = useMemo(() => {
    const tokens = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (tokens.length === 0) return commands;
    return commands
      .map((c) => ({ c, s: rank(c, tokens) }))
      .filter((x): x is { c: Command; s: number } => x.s !== null)
      .sort((a, b) => a.s - b.s)
      .map((x) => x.c);
  }, [commands, q]);

  useEffect(() => setActive(0), [q]);
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Keep the active row in view as the user arrows through.
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${active}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const choose = (i: number) => {
    const cmd = results[i];
    if (!cmd) return;
    onClose();
    cmd.run();
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, results.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === "Home") {
      e.preventDefault();
      setActive(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setActive(results.length - 1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(active);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  let lastGroup: string | undefined;

  return (
    <div className="cmdk-overlay" onClick={onClose}>
      <div
        ref={cardRef}
        className="cmdk"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="cmdk-input">
          <IconSearch size={18} />
          <input
            ref={inputRef}
            type="text"
            placeholder="Jump to a page, camera, or action…"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={onKey}
            aria-activedescendant={results[active] ? `cmdk-${results[active].id}` : undefined}
            aria-controls="cmdk-list"
            role="combobox"
            aria-expanded="true"
          />
          <kbd className="cmdk-kbd">Esc</kbd>
        </div>
        <div className="cmdk-list" id="cmdk-list" role="listbox" ref={listRef}>
          {results.length === 0 && (
            <div className="cmdk-empty">
              No matches{q.trim() ? ` for “${q.trim()}”` : ""} — try a page, camera, or action name.
            </div>
          )}
          {results.map((c, i) => {
            const showGroup = c.group && c.group !== lastGroup;
            lastGroup = c.group;
            return (
              <div key={c.id}>
                {showGroup && <div className="cmdk-group eyebrow">{c.group}</div>}
                <div
                  id={`cmdk-${c.id}`}
                  data-idx={i}
                  role="option"
                  aria-selected={i === active}
                  className={`cmdk-item ${i === active ? "active" : ""}`}
                  onMouseMove={() => setActive(i)}
                  onClick={() => choose(i)}
                >
                  {c.icon && <span className="cmdk-ico">{c.icon}</span>}
                  <span className="cmdk-label">{c.label}</span>
                  {c.hint && <span className="cmdk-hint">{c.hint}</span>}
                </div>
              </div>
            );
          })}
        </div>
        <div className="cmdk-foot">
          <span><kbd className="cmdk-kbd">↑</kbd><kbd className="cmdk-kbd">↓</kbd> navigate</span>
          <span><kbd className="cmdk-kbd">↵</kbd> run</span>
          <span><kbd className="cmdk-kbd">esc</kbd> close</span>
        </div>
      </div>
    </div>
  );
}
