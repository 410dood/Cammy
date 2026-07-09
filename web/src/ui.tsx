// Lightweight, dependency-free UI primitives that replace the browser's
// window.alert / confirm / prompt and provide a consistent modal + toast layer.
//
// Why: native dialogs are jarring, unstyleable, and break the dark theme; toasts
// give non-blocking, accessible feedback. All of this is plain React + CSS tokens.
//
// Usage:
//   const toast = useToast();           toast.success("Saved");
//   const dialog = useDialog();         if (await dialog.confirm({ ... })) {...}
//                                       const note = await dialog.prompt({ ... });
//   <Modal onClose={...} title="...">…</Modal>   // accessible media/content modal

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  ReactNode,
  RefObject,
  CSSProperties,
} from "react";
import { IconCheck, IconAlert, IconInfo, IconX } from "./icons";
import { relTime, fmtTime } from "./api";

/* ======================================================================== */
/* useFocusTrap — keep Tab focus inside an open overlay                       */
/* ======================================================================== */

const FOCUSABLE =
  'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),' +
  'textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

/** Cycle Tab / Shift-Tab within `ref`'s subtree so keyboard focus can't escape
 *  an open modal/panel into the page behind it. The element should already hold
 *  focus (callers focus an input, the card, or the container on open). */
export function useFocusTrap(ref: RefObject<HTMLElement>) {
  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const items = Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
        (el) => el.offsetWidth > 0 || el.offsetHeight > 0 || el === document.activeElement,
      );
      if (items.length === 0) {
        e.preventDefault();
        node.focus();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (e.shiftKey) {
        if (active === first || !node.contains(active)) {
          e.preventDefault();
          last.focus();
        }
      } else if (active === last || !node.contains(active)) {
        e.preventDefault();
        first.focus();
      }
    };
    node.addEventListener("keydown", onKey);
    return () => node.removeEventListener("keydown", onKey);
  }, [ref]);
}

/* ======================================================================== */
/* RelTime — self-updating relative timestamp                                */
/* ======================================================================== */

// One shared 30s ticker drives every <RelTime>, so a feed of 100 cards spins a
// single timer instead of 100. Subscribers are re-rendered together.
const nowSubs = new Set<() => void>();
let nowTimer: ReturnType<typeof setInterval> | null = null;
function subscribeNow(cb: () => void): () => void {
  nowSubs.add(cb);
  if (!nowTimer) {
    nowTimer = setInterval(() => nowSubs.forEach((f) => f()), 30_000);
  }
  return () => {
    nowSubs.delete(cb);
    if (nowSubs.size === 0 && nowTimer) {
      clearInterval(nowTimer);
      nowTimer = null;
    }
  };
}

/** A `<time>` showing compact relative text ("4m ago") that refreshes itself,
 *  with the absolute timestamp on hover (title) and in `dateTime`. */
export function RelTime({
  ts,
  className,
  prefix,
  style,
}: {
  ts: number;
  className?: string;
  prefix?: string;
  style?: CSSProperties;
}) {
  const [, force] = useState(0);
  useEffect(() => subscribeNow(() => force((n) => n + 1)), []);
  return (
    <time
      className={className}
      style={style}
      dateTime={new Date(ts * 1000).toISOString()}
      title={fmtTime(ts)}
    >
      {prefix}
      {relTime(ts)}
    </time>
  );
}

/* ======================================================================== */
/* EmptyState — a friendly, branded "nothing here yet" block                 */
/* ======================================================================== */

/** Centered icon + title + hint (+ optional action) for empty lists. Replaces
 *  bare "No X yet" text so empty surfaces feel intentional, not broken. */
export function EmptyState({
  icon,
  title,
  hint,
  action,
  tone,
}: {
  icon?: ReactNode;
  title: string;
  hint?: ReactNode;
  action?: ReactNode;
  /** Tints the icon for non-neutral states (e.g. a failed load). */
  tone?: "danger" | "warn";
}) {
  return (
    <div className="empty-state">
      {icon && (
        <div className={`empty-state-ico${tone ? ` ${tone}` : ""}`} aria-hidden="true">
          {icon}
        </div>
      )}
      <div className="empty-state-title">{title}</div>
      {hint && <p className="empty-state-hint">{hint}</p>}
      {action && <div className="empty-state-action">{action}</div>}
    </div>
  );
}

/** A failed-to-load state — distinct from a genuinely-empty one — with a Retry.
 *  Use this when a fetch errors so the surface doesn't lie "Nothing here yet." */
export function ErrorState({
  what,
  message,
  onRetry,
}: {
  what: string;
  message?: string | null;
  onRetry?: () => void;
}) {
  return (
    <EmptyState
      tone="danger"
      icon={<IconAlert />}
      title={`Couldn't load ${what}`}
      hint={message || "The server didn't respond. Check that it's running and reachable, then retry."}
      action={
        onRetry ? (
          <button className="btn btn-secondary" onClick={onRetry}>
            Retry
          </button>
        ) : undefined
      }
    />
  );
}

/* ======================================================================== */
/* Callout — a toned notice box (info/warn/danger) with a leading icon        */
/* ======================================================================== */

/** One shared shape for the `.callout` recipe so pages stop hand-assembling
 *  the icon + role + tone markup (which had already drifted on role/aria). */
export function Callout({
  tone = "info",
  role,
  icon,
  className,
  style,
  children,
}: {
  tone?: "info" | "warn" | "danger";
  /** Defaults to "alert" for danger, "status" otherwise. */
  role?: string;
  /** Defaults to IconInfo for info, IconAlert for warn/danger. */
  icon?: ReactNode;
  className?: string;
  style?: CSSProperties;
  children: ReactNode;
}) {
  return (
    <div
      className={`callout callout-${tone}${className ? ` ${className}` : ""}`}
      role={role ?? (tone === "danger" ? "alert" : "status")}
      style={style}
    >
      <span className="callout-ico">{icon ?? (tone === "info" ? <IconInfo size={16} /> : <IconAlert size={16} />)}</span>
      <div>{children}</div>
    </div>
  );
}

/* ======================================================================== */
/* TogglePill — an accessible on/off pill (real <button>, keyboard + SR)      */
/* ======================================================================== */

/** A pill-shaped toggle that renders a real, focusable `<button>` with
 *  `aria-pressed`, so it is keyboard-operable (Enter/Space) and announced as a
 *  toggle button to screen readers. Replaces the `<span className="pill toggle"
 *  onClick>` pattern, which was mouse-only and invisible to assistive tech.
 *  Visuals come from the existing `.pill.toggle` recipe in styles.css. */
export function TogglePill({
  on,
  onClick,
  children,
  title,
  className,
  ariaLabel,
  disabled,
}: {
  on: boolean;
  onClick: () => void;
  children: ReactNode;
  title?: string;
  className?: string;
  ariaLabel?: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className={`pill toggle${on ? " on" : ""}${className ? ` ${className}` : ""}`}
      aria-pressed={on}
      aria-label={ariaLabel}
      title={title}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

/* ======================================================================== */
/* Toasts                                                                    */
/* ======================================================================== */

type ToastKind = "success" | "error" | "info";
interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

interface ToastApi {
  push: (message: string, kind?: ToastKind) => void;
  success: (message: string) => void;
  error: (message: string) => void;
  info: (message: string) => void;
}

const ToastCtx = createContext<ToastApi | null>(null);

let toastSeq = 1;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  // Per-toast auto-dismiss timers, tracked so we can PAUSE them while the user
  // hovers or focuses the stack (WCAG 2.2.1 Timing Adjustable, Level A — the
  // close button must not be the only way to beat a 4–6s timeout).
  const timers = useRef<Map<number, { handle: number; remaining: number; startedAt: number }>>(new Map());

  const remove = useCallback((id: number) => {
    const t = timers.current.get(id);
    if (t) {
      window.clearTimeout(t.handle);
      timers.current.delete(id);
    }
    setToasts((ts) => ts.filter((x) => x.id !== id));
  }, []);

  const arm = useCallback(
    (id: number, ms: number) => {
      timers.current.set(id, { handle: window.setTimeout(() => remove(id), ms), remaining: ms, startedAt: Date.now() });
    },
    [remove],
  );

  const push = useCallback(
    (message: string, kind: ToastKind = "info") => {
      const id = toastSeq++;
      setToasts((t) => [...t, { id, kind, message }]);
      // Errors linger a little longer; everything auto-dismisses (paused on hover/focus).
      arm(id, kind === "error" ? 6000 : 4000);
    },
    [arm],
  );

  const pauseAll = useCallback(() => {
    const now = Date.now();
    for (const t of timers.current.values()) {
      window.clearTimeout(t.handle);
      t.remaining = Math.max(1200, t.remaining - (now - t.startedAt));
    }
  }, []);
  const resumeAll = useCallback(() => {
    const now = Date.now();
    for (const [id, t] of timers.current) {
      t.startedAt = now;
      t.handle = window.setTimeout(() => remove(id), t.remaining);
    }
  }, [remove]);

  const api: ToastApi = {
    push,
    success: (m) => push(m, "success"),
    error: (m) => push(m, "error"),
    info: (m) => push(m, "info"),
  };

  const renderToast = (t: Toast) => (
    <div key={t.id} className={`toast toast-${t.kind}`}>
      <span className="toast-ico">
        {t.kind === "success" ? <IconCheck size={16} /> : t.kind === "error" ? <IconAlert size={16} /> : <IconInfo size={16} />}
      </span>
      <span className="toast-msg">{t.message}</span>
      <button className="toast-close" aria-label="Dismiss" onClick={() => remove(t.id)}>
        <IconX size={14} />
      </button>
    </div>
  );

  // Latest message per urgency, mirrored into always-mounted screen-reader live
  // regions so errors announce assertively instead of queueing behind info/success.
  const lastError = toasts.filter((t) => t.kind === "error").slice(-1)[0]?.message ?? "";
  const lastPolite = toasts.filter((t) => t.kind !== "error").slice(-1)[0]?.message ?? "";

  return (
    <ToastCtx.Provider value={api}>
      {children}
      <div
        className="toast-host"
        role="region"
        aria-label="Notifications"
        onMouseEnter={pauseAll}
        onMouseLeave={resumeAll}
        onFocus={pauseAll}
        onBlur={resumeAll}
      >
        {toasts.map(renderToast)}
      </div>
      <div className="sr-only" role="alert" aria-live="assertive">{lastError}</div>
      <div className="sr-only" role="status" aria-live="polite">{lastPolite}</div>
    </ToastCtx.Provider>
  );
}

export function useToast(): ToastApi {
  const ctx = useContext(ToastCtx);
  if (!ctx) throw new Error("useToast must be used within <ToastProvider>");
  return ctx;
}

/* ======================================================================== */
/* Accessible modal (media lightbox / content dialog)                        */
/* ======================================================================== */

export function Modal({
  children,
  onClose,
  title,
  className,
  bare,
}: {
  children: ReactNode;
  onClose: () => void;
  title?: string;
  className?: string;
  /** bare = no card chrome (used for full-bleed media lightboxes). */
  bare?: boolean;
}) {
  const labelId = useId();
  const cardRef = useRef<HTMLDivElement>(null);
  useFocusTrap(cardRef);

  // Escape-to-close. Keyed on onClose, which callers often pass as an inline
  // arrow, so this effect may re-run on every parent render — that's fine, it
  // only swaps a window listener.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Move focus into the dialog on open and return it to the triggering element
  // on close. EMPTY deps on purpose: this must run only on mount/unmount.
  // Callers typically pass an inline onClose whose identity changes every
  // render, and parent pages poll on intervals — keying focus handling on
  // onClose would re-run it on every render and repeatedly yank focus out of
  // the modal's own fields (unusable for keyboard / screen-reader users).
  useEffect(() => {
    const prevFocus = document.activeElement as HTMLElement | null;
    cardRef.current?.focus();
    return () => {
      // Restore only to a still-connected trigger we don't already hold, so a
      // removed trigger or focus intentionally moved elsewhere isn't clobbered.
      if (prevFocus && prevFocus.isConnected && document.activeElement !== prevFocus) {
        prevFocus.focus?.();
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="modal-bg" onClick={onClose}>
      <div
        ref={cardRef}
        className={`modal-card ${bare ? "modal-bare" : ""} ${className ?? ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        aria-labelledby={title ? labelId : undefined}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
      >
        {title && !bare && (
          <div className="modal-head">
            <h2 id={labelId}>{title}</h2>
            <button className="icon-btn" aria-label="Close" onClick={onClose}>
              <IconX size={18} />
            </button>
          </div>
        )}
        {children}
      </div>
    </div>
  );
}

/* ======================================================================== */
/* Promise-based confirm / prompt dialogs                                    */
/* ======================================================================== */

interface ConfirmOpts {
  title: string;
  body?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
}
interface PromptOpts {
  title: string;
  body?: ReactNode;
  label?: string;
  defaultValue?: string;
  placeholder?: string;
  confirmLabel?: string;
  maxLength?: number;
  multiline?: boolean;
}

interface DialogApi {
  confirm: (opts: ConfirmOpts) => Promise<boolean>;
  prompt: (opts: PromptOpts) => Promise<string | null>;
}

const DialogCtx = createContext<DialogApi | null>(null);

type Pending =
  | { type: "confirm"; opts: ConfirmOpts; resolve: (v: boolean) => void }
  | { type: "prompt"; opts: PromptOpts; resolve: (v: string | null) => void };

export function DialogProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<Pending | null>(null);

  const api: DialogApi = {
    confirm: (opts) => new Promise<boolean>((resolve) => setPending({ type: "confirm", opts, resolve })),
    prompt: (opts) => new Promise<string | null>((resolve) => setPending({ type: "prompt", opts, resolve })),
  };

  const close = (value: boolean | string | null) => {
    if (!pending) return;
    if (pending.type === "confirm") pending.resolve(value as boolean);
    else pending.resolve(value as string | null);
    setPending(null);
  };

  return (
    <DialogCtx.Provider value={api}>
      {children}
      {pending && <DialogHost pending={pending} onClose={close} />}
    </DialogCtx.Provider>
  );
}

function DialogHost({ pending, onClose }: { pending: Pending; onClose: (v: boolean | string | null) => void }) {
  const isPrompt = pending.type === "prompt";
  const promptOpts = isPrompt ? (pending.opts as PromptOpts) : null;
  const [value, setValue] = useState(promptOpts?.defaultValue ?? "");
  const inputRef = useRef<HTMLInputElement & HTMLTextAreaElement>(null);

  useEffect(() => {
    const t = window.setTimeout(() => {
      inputRef.current?.focus();
      inputRef.current?.select?.();
    }, 30);
    return () => window.clearTimeout(t);
  }, []);

  const cancelValue = isPrompt ? null : false;
  const confirm = () => onClose(isPrompt ? value : true);

  return (
    <Modal title={pending.opts.title} onClose={() => onClose(cancelValue)}>
      <div className="dialog-body">
        {pending.opts.body && <div className="muted">{pending.opts.body}</div>}
        {isPrompt && promptOpts && (
          <label className="field" style={{ width: "100%" }}>
            {promptOpts.label}
            {promptOpts.multiline ? (
              <textarea
                ref={inputRef}
                rows={3}
                value={value}
                placeholder={promptOpts.placeholder}
                maxLength={promptOpts.maxLength}
                onChange={(e) => setValue(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) confirm();
                }}
              />
            ) : (
              <input
                ref={inputRef}
                type="text"
                value={value}
                placeholder={promptOpts.placeholder}
                maxLength={promptOpts.maxLength}
                onChange={(e) => setValue(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && confirm()}
              />
            )}
          </label>
        )}
      </div>
      <div className="dialog-actions">
        <button className="btn btn-ghost" onClick={() => onClose(cancelValue)}>
          {pending.type === "confirm" ? pending.opts.cancelLabel ?? "Cancel" : "Cancel"}
        </button>
        <button
          className={`btn ${pending.type === "confirm" && pending.opts.danger ? "btn-danger-solid" : "btn-primary"}`}
          onClick={confirm}
        >
          {pending.type === "confirm"
            ? pending.opts.confirmLabel ?? "Confirm"
            : promptOpts?.confirmLabel ?? "Save"}
        </button>
      </div>
    </Modal>
  );
}

export function useDialog(): DialogApi {
  const ctx = useContext(DialogCtx);
  if (!ctx) throw new Error("useDialog must be used within <DialogProvider>");
  return ctx;
}
