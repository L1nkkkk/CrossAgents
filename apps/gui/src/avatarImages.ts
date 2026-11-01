// Gallery pictures as the page shows them. A picture's id is its content hash, so a loaded one
// never changes; failures are forgotten after a while, so a hub that comes back heals the avatar.

import { useEffect, useSyncExternalStore } from "react";
import { api } from "./api";
import { useHubData } from "./hub";

const RETRY_MS = 30_000;
const urls = new Map<string, string>();
const asked = new Set<string>();
const listeners = new Set<() => void>();

function request(id: string) {
  if (urls.has(id) || asked.has(id)) return;
  asked.add(id);
  api.avatar(id).then(
    (content) => {
      urls.set(id, `data:${content.mime};base64,${content.data}`);
      listeners.forEach((l) => l());
    },
    () => {
      setTimeout(() => asked.delete(id), RETRY_MS);
    },
  );
}

/** The picture's data URL once it is loaded; a `data:` image is what the page's CSP allows. */
export function useAvatarImage(id: string | null | undefined): string | undefined {
  useEffect(() => {
    if (id) request(id);
  }, [id]);
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => (id ? urls.get(id) : undefined),
  );
}

/** The gallery's picture ids, for the settings page and the profile editor. */
export function useAvatarGallery() {
  return useHubData(() => api.listAvatars().then((r) => r.avatars), [], () => false);
}

const AVATAR_SIZE = 128;

/**
 * A chosen file as a small centred square, base64: re-encoding keeps uploads small and leaves
 * behind whatever else the file carried (EXIF location, for one).
 */
export async function avatarFromFile(file: File): Promise<string> {
  const bitmap = await createImageBitmap(file);
  const side = Math.min(bitmap.width, bitmap.height);
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = AVATAR_SIZE;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("no canvas");
  context.imageSmoothingQuality = "high";
  context.drawImage(bitmap, (bitmap.width - side) / 2, (bitmap.height - side) / 2, side, side, 0, 0, AVATAR_SIZE, AVATAR_SIZE);
  bitmap.close();
  return canvas.toDataURL("image/webp", 0.86).split(",")[1];
}
