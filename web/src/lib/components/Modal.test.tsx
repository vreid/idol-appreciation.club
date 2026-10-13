// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import Modal from "./Modal";

afterEach(() => {
  cleanup();
});

describe("Modal", () => {
  it("renders the title and children", () => {
    render(() => (
      <Modal
        title="About"
        onClose={() => {
          // noop: no behavior under test here
        }}
      >
        <p>body copy</p>
      </Modal>
    ));
    expect(screen.getByText("About")).toBeDefined();
    expect(screen.getByText("body copy")).toBeDefined();
  });

  it("Escape closes via the window listener", () => {
    const onClose = vi.fn((): void => {});
    render(() => (
      <Modal title="About" onClose={onClose}>
        x
      </Modal>
    ));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("other keys never reach the window listener", () => {
    const onClose = vi.fn((): void => {});
    render(() => (
      <Modal title="About" onClose={onClose}>
        <div>focusable</div>
      </Modal>
    ));
    const dialog = screen.getByRole("dialog");
    dialog.focus();
    fireEvent.keyDown(dialog, { key: "Tab" });
    fireEvent.keyDown(window, { key: "Tab" });
    expect(onClose).not.toHaveBeenCalled();
  });

  it("clicking the backdrop closes but the dialog swallows clicks", () => {
    const onClose = vi.fn((): void => {});
    render(() => (
      <Modal title="About" onClose={onClose}>
        <p>body</p>
      </Modal>
    ));
    fireEvent.click(screen.getByRole("dialog"));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("presentation"));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("focuses the dialog on mount for Escape accessibility", async () => {
    render(() => (
      <Modal title="About" onClose={() => {}}>
        x
      </Modal>
    ));
    await Promise.resolve();
    expect(document.activeElement).toBe(screen.getByRole("dialog"));
  });
});
