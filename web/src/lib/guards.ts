/**
 * The app's single canonical object guard. Response shapes are validated
 * field-by-field with typeof checks in api.ts on top of this guard; a
 * schema-validation dependency would cost more than it saves at this size.
 */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
