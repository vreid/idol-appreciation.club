import { For } from "solid-js";
import type { Toast } from "../toasts";

export default function Toasts(props: {
  toasts: readonly Toast[];
  onDismiss: (id: number) => void;
}) {
  return (
    <div class="toasts" role="status" aria-live="polite">
      <For each={props.toasts}>
        {(toast) => (
          <button
            class={`toast ${toast.kind}`}
            type="button"
            onClick={() => {
              props.onDismiss(toast.id);
            }}
          >
            {toast.message}
          </button>
        )}
      </For>
    </div>
  );
}
