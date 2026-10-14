import { describe, expect, it, vi } from "vite-plus/test";
import { ApiError, type ApiClient, type DigEntry, type Placement } from "./api";
import type { ToastKind } from "./toasts";
import { createTagsEditor, type TagsEditorDeps } from "./tagsEdit";

const PLACEMENT: Placement = {
  id: "p1",
  username: "wren",
  x: 10,
  y: 10,
  w: 64,
  h: 48,
  rot: 0,
  created_at: "2026-10-13T00:00:00Z",
  tags: ["stage"],
};

function makeDeps(overrides: Partial<TagsEditorDeps> = {}): TagsEditorDeps & {
  applied: { id: string; tags: string[] }[];
  errors: string[];
} {
  const applied: { id: string; tags: string[] }[] = [];
  const errors: string[] = [];
  const client: Pick<ApiClient, "updateTags"> = {
    updateTags: vi.fn(() => Promise.resolve({ id: "p1", tags: ["encore"] })),
  };
  return {
    client,
    requireAuth: vi.fn(() => true),
    toast: vi.fn((kind: ToastKind, message: string) => {
      void kind;
      errors.push(message);
    }),
    selected: vi.fn(() => PLACEMENT),
    setSelected: vi.fn((_p: Placement | null) => {}),
    setStack: vi.fn((patch: (stack: DigEntry[]) => DigEntry[]) => {
      applied.push({
        id: "stack",
        tags: patch([{ ...PLACEMENT, depth: 1 }])[0]?.tags ?? [],
      });
    }),
    ...overrides,
    applied,
    errors,
  };
}

describe("tagsEdit", () => {
  it("gates edits behind the login prompt", async () => {
    const deps = makeDeps({ requireAuth: () => false });
    const editor = createTagsEditor(deps);
    const result = await editor.update("p1", ["new"]);
    expect(deps.client.updateTags).not.toHaveBeenCalled();
    expect(result).toBeNull();
  });

  it("applies the authoritative list to selection and stack", async () => {
    const setSelected = vi.fn((_p: Placement | null) => {});
    const deps = makeDeps({ setSelected });
    const editor = createTagsEditor(deps);
    const result = await editor.update("p1", ["stage", "encore"]);
    expect(deps.client.updateTags).toHaveBeenCalledWith("p1", [
      "stage",
      "encore",
    ]);
    expect(result?.tags).toEqual(["encore"]);
    expect(setSelected).toHaveBeenCalledWith({
      ...PLACEMENT,
      tags: ["encore"],
    });
    expect(deps.applied[0]?.tags).toEqual(["encore"]);
  });

  it("leaves a different selection untouched", async () => {
    const setSelected = vi.fn((_p: Placement | null) => {});
    const deps = makeDeps({
      setSelected,
      selected: () => null,
    });
    const editor = createTagsEditor(deps);
    await editor.update("p1", ["x"]);
    expect(setSelected).not.toHaveBeenCalled();
  });

  it("sends 401/403 to the auth prompt without an error toast", async () => {
    const deps = makeDeps({
      client: {
        updateTags: () => Promise.reject(new ApiError(403, "no", null)),
      },
    });
    const editor = createTagsEditor(deps);
    const result = await editor.update("p1", ["x"]);
    expect(result).toBeNull();
    // Gate + the 401 re-check (which re-prompts in the store).
    expect(deps.requireAuth).toHaveBeenCalledTimes(2);
    expect(deps.errors).toEqual([]);
  });

  it("toasts other failures and changes nothing", async () => {
    const setSelected = vi.fn((_p: Placement | null) => {});
    const deps = makeDeps({
      setSelected,
      client: {
        updateTags: () => Promise.reject(new ApiError(400, "bad tag", null)),
      },
    });
    const editor = createTagsEditor(deps);
    const result = await editor.update("p1", ["x".repeat(33)]);
    expect(result).toBeNull();
    expect(deps.errors).toEqual(["bad tag"]);
    expect(setSelected).not.toHaveBeenCalled();
  });
});
