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
  CSSProperties,
} from "react";
import { IconCheck, IconAlert, IconInfo, IconX } from "./icons";
import { relTime, fmtTime } from "./api";

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

  const remove = useCallback((id: number) => {
    setToasts((t) => t.filter((x) => x.id !== id));
  }, []);

  const push = useCallback(
    (message: string, kind: ToastKind = "info") => {
      const id = toastSeq++;
      setToasts((t) => [...t, { id, kind, message }]);
      // Errors linger a little longer; everything auto-dismisses.
      window.setTimeout(() => remove(id), kind === "error" ? 6000 : 4000);
    },
    [remove],
  );

  const api: ToastApi = {
    push,
    success: (m) => push(m, "success"),
    error: (m) => push(m, "error"),
    info: (m) => push(m, "info"),
  };

  return (
    <ToastCtx.Provider value={api}>
      {children}
      <div className="toast-host" role="region" aria-live="polite" aria-label="Notifications">
        {toasts.map((t) => (
          <div key={t.id} className={`toast toast-${t.kind}`}>
            <span className="toast-ico">
              {t.kind === "success" ? <IconCheck size={16} /> : t.kind === "error" ? <IconAlert size={16} /> : <IconInfo size={16} />}
            </span>
            <span className="toast-msg">{t.message}</span>
            <button className="toast-close" aria-label="Dismiss" onClick={() => remove(t.id)}>
              <IconX size={14} />
            </button>
          </div>
        ))}
      </div>
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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    cardRef.current?.focus();
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

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
