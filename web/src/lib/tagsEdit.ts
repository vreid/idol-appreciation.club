import type { ApiClient, DigEntry, Placement, TagList } from "./api";
import type { ToastKind } from "./toasts";
import { describeApiError, isAuthStatus } from "./apiErrors";

/** Everything the tags editor needs from the store. Injected so the
 * logic is testable without Solid signals. */
export interface TagsEditorDeps {
  client: Pick<ApiClient, "updateTags">;
  requireAuth: () => boolean;
  toast: (kind: ToastKind, message: string) => void;
  selected: () => Placement | null;
  setSelected: (placement: Placement | null) => void;
  setStack: (patch: (stack: DigEntry[]) => DigEntry[]) => void;
}

export interface TagsEditor {
  /** Replaces a placement's tag set (owner or admin). On success the
   * authoritative normalized list is applied to the selection and the
   * dig stack and returned; null means the edit failed — a toast or the
   * auth prompt already fired. */
  update(placementId: string, tags: string[]): Promise<TagList | null>;
}

export function createTagsEditor(deps: TagsEditorDeps): TagsEditor {
  return {
    async update(placementId, tags) {
      if (!deps.requireAuth()) return null;
      try {
        const result = await deps.client.updateTags(placementId, tags);
        const sel = deps.selected();
        if (sel !== null && sel.id === placementId) {
          deps.setSelected({ ...sel, tags: result.tags });
        }
        deps.setStack((stack) =>
          stack.map((e) =>
            e.id === placementId ? { ...e, tags: result.tags } : e,
          ),
        );
        return result;
      } catch (err) {
        if (isAuthStatus(err)) deps.requireAuth();
        else deps.toast("error", describeApiError(err));
        return null;
      }
    },
  };
}
