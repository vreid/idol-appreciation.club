export type ToastKind = "info" | "error" | "success";

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

const TOAST_MS = 4000;
const ERROR_TOAST_MS = 6000;

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

  let toasts = $state<Toast[]>([]);
  let nextId = 1;

  function dismiss(id: number): void {
    toasts = toasts.filter((toast) => toast.id !== id);
  }

  function push(kind: ToastKind, message: string): void {
    const id = nextId++;
    toasts.push({ id, kind, message });
    setTimeout(
      () => {
        dismiss(id);
      },
      kind === "error" ? errorToastMs : toastMs,
    );
  }

  return {
    get list() {
      return toasts;
    },
    push,
    dismiss,
  };
}
