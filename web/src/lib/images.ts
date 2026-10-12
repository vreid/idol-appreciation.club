/** DOM-only helpers for the place flow. Excluded from unit coverage. */

export function readNaturalSize(
  file: Blob,
): Promise<{ w: number; h: number } | null> {
  const { promise, resolve } = Promise.withResolvers<{
    w: number;
    h: number;
  } | null>();
  const url = URL.createObjectURL(file);
  const img = new Image();
  img.addEventListener("load", () => {
    URL.revokeObjectURL(url);
    resolve({ w: img.naturalWidth, h: img.naturalHeight });
  });
  img.addEventListener("error", () => {
    URL.revokeObjectURL(url);
    resolve(null);
  });
  img.src = url;
  return promise;
}
