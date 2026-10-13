/**
 * The app's single canonical object guard. REST and SSE payloads are
 * validated at the boundary with the wire-contract Zod schemas
 * (contract/zod, via `@contract`); this guard remains for Leaflet-internal
 * and DOM-adjacent objects that no schema covers (see tileDom.ts).
 */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
