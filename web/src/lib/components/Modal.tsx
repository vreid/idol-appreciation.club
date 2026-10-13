import { onCleanup, onMount, type JSX } from "solid-js";

// Native listener, not Solid's delegated one: the dialog must swallow its
// own key events at the element phase, while Escape keeps bubbling to the
// window listener that closes the modal.
function onDialogKeydown(event: KeyboardEvent): void {
  if (event.key !== "Escape") event.stopPropagation();
}

export default function Modal(props: {
  title: string;
  onClose: () => void;
  children: JSX.Element;
}) {
  let dialogEl: HTMLDivElement | undefined;

  function onWindowKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") props.onClose();
  }

  onMount(() => {
    dialogEl?.focus();
    dialogEl?.addEventListener("keydown", onDialogKeydown);
    window.addEventListener("keydown", onWindowKeydown);
    onCleanup(() => {
      dialogEl?.removeEventListener("keydown", onDialogKeydown);
      window.removeEventListener("keydown", onWindowKeydown);
    });
  });

  return (
    <div class="modal-backdrop" onClick={props.onClose} role="presentation">
      <div
        class="modal card"
        role="dialog"
        aria-modal="true"
        aria-label={props.title}
        tabindex={-1}
        ref={dialogEl}
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div class="modal-head">
          <h2>{props.title}</h2>
          <button class="btn ghost small" type="button" onClick={props.onClose}>
            ✕
          </button>
        </div>
        {props.children}
      </div>
    </div>
  );
}
