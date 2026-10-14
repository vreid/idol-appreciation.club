import { createSignal } from "solid-js";
import type { ApiClient, DigEntry, Placement } from "./api";

/** The pile (dig stack) under the current selection: cached strata for
 * the panel's depth steppers and the repeat-click cycle. */
export interface PileState {
  stack(): DigEntry[];
  total(): number;
  index(): number;
  /** Digs the pile under a point and caches it; true when the current
   * selection is one of its strata (the repeat-click cycle case). */
  query(x: number, y: number): Promise<boolean>;
  /** Selects `top` against the pile cached by query (no refetch). */
  adopt(top: Placement): void;
  /** One stratum down/up through the cached pile (wrapping). False
   * when there is nothing to step through. */
  step(delta: number): boolean;
  /** Loads the pile under `point` for `placement` (jump paths; the
   * fetch is dropped if the selection moved on). */
  load(placement: Placement, point: { x: number; y: number }): void;
  reset(): void;
}

export function createPileState(deps: {
  client: Pick<ApiClient, "dig">;
  selected: () => Placement | null;
  setSelected: (placement: Placement) => void;
}): PileState {
  const [stack, setStack] = createSignal<DigEntry[]>([]);
  const [total, setTotal] = createSignal(0);
  const [index, setIndex] = createSignal(-1);

  const reset = (): void => {
    setStack([]);
    setTotal(0);
    setIndex(-1);
  };

  return {
    stack,
    total,
    index,
    reset,
    load(placement, point) {
      reset();
      // A failed dig degrades silently; a superseded one is dropped.
      void deps.client.dig(point.x, point.y).then((page) => {
        if (deps.selected()?.id !== placement.id) return;
        setStack(page.stack);
        setTotal(page.total);
        setIndex(page.stack.findIndex((e) => e.id === placement.id));
      });
    },
    async query(x, y) {
      const current = deps.selected();
      try {
        const page = await deps.client.dig(x, y);
        setStack(page.stack);
        setTotal(page.total);
        setIndex(
          current === null
            ? -1
            : page.stack.findIndex((e) => e.id === current.id),
        );
        return current !== null && index() >= 0;
      } catch {
        reset();
        return false;
      }
    },
    adopt(top) {
      deps.setSelected(top);
      setIndex(stack().findIndex((e) => e.id === top.id));
    },
    step(delta) {
      const entries = stack();
      if (entries.length < 2) return false;
      const next = (index() + delta + entries.length) % entries.length;
      setIndex(next);
      const nextPlacement = entries[next];
      if (nextPlacement !== undefined) deps.setSelected(nextPlacement);
      return true;
    },
  };
}
