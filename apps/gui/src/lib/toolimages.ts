/** Extract embedded images while preserving all other result content. */
export function extractToolImages(raw: string): { images: string[]; text: string } {
  const images: string[] = [];
  const imageUrl = (s: string) => /^data:image\/[a-z0-9.+-]+;base64,/i.test(s);
  if (imageUrl(raw)) return { images: [raw], text: "" };
  let value: unknown;
  try { value = JSON.parse(raw); } catch { return { images, text: raw }; }
  const remove = (v: unknown): unknown => {
    if (typeof v === "string" && imageUrl(v)) { images.push(v); return undefined; }
    if (Array.isArray(v)) return v.map(remove).filter((x) => x !== undefined);
    if (v && typeof v === "object") {
      const o = v as Record<string, unknown>;
      if (o.type === "image" && typeof o.data === "string" && typeof o.mimeType === "string" && /^image\/[a-z0-9.+-]+$/i.test(o.mimeType)) {
        images.push(`data:${o.mimeType};base64,${o.data}`); return undefined;
      }
      if (o.type === "image_url" && o.image_url && typeof o.image_url === "object" && typeof (o.image_url as Record<string, unknown>).url === "string" && imageUrl((o.image_url as { url: string }).url)) {
        images.push((o.image_url as { url: string }).url); return undefined;
      }
      return Object.fromEntries(Object.entries(o).map(([k, x]) => [k, remove(x)]).filter(([, x]) => x !== undefined));
    }
    return v;
  };
  const rest = remove(value);
  if (!images.length) return { images, text: raw };
  if (rest === undefined || (Array.isArray(rest) && !rest.length)) return { images, text: "" };
  return { images, text: typeof rest === "string" ? rest : JSON.stringify(rest, null, 2) };
}
