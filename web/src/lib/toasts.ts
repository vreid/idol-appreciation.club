import { createSignal } from "solid-js";

export type ToastKind = "info" | "error" | "success";

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

const TOAST_MS = 4000;
const ERROR_TOAST_MS = 6000;

/** Maximum number of toasts visible at once. */
export const MAX_VISIBLE_TOASTS = 3;

export interface ToastHub {
  /** Reactive list — read from components, never mutate directly. */
  readonly list: Toast[];
  push(this: void, kind: ToastKind, message: string): void;
  dismiss(this: void, id: number): void;
}

export function createToastHub(
  deps: {
    toastMs?: number | undefined;
    errorToastMs?: number | undefined;
  } = {},
): ToastHub {
  const toastMs = deps.toastMs ?? TOAST_MS;
  const errorToastMs = deps.errorToastMs ?? ERROR_TOAST_MS;

  const [toasts, setToasts] = createSignal<Toast[]>([]);
  let nextId = 1;
  const timers = new Map<number, ReturnType<typeof setTimeout>>();

  function arm(id: number, kind: ToastKind): void {
    clearTimeout(timers.get(id));
    timers.set(
      id,
      setTimeout(
        () => {
          timers.delete(id);
          dismiss(id);
        },
        kind === "error" ? errorToastMs : toastMs,
      ),
    );
  }

  function dismiss(id: number): void {
    clearTimeout(timers.get(id));
    timers.delete(id);
    setToasts((list) => list.filter((toast) => toast.id !== id));
  }

  function push(kind: ToastKind, message: string): void {
    // Dedupe: a repeated message re-arms the visible pill instead of
    // stacking an identical one.
    const existing = toasts().find(
      (toast) => toast.kind === kind && toast.message === message,
    );
    if (existing !== undefined) {
      arm(existing.id, kind);
      return;
    }
    const id = nextId++;
    setToasts((list) => [...list, { id, kind, message }]);
    arm(id, kind);
    // Hard cap: drop the oldest toasts beyond the visible limit.
    while (toasts().length > MAX_VISIBLE_TOASTS) {
      const oldest = toasts()[0];
      if (oldest === undefined) break;
      dismiss(oldest.id);
    }
  }

  return {
    get list() {
      return toasts();
    },
    push,
    dismiss,
  };
}
