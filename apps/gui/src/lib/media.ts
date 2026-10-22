import { useSyncExternalStore } from "react";

/** Whether a media query matches, kept current. */
export function useMedia(query: string): boolean {
  return useSyncExternalStore(
    (l) => {
      const m = matchMedia(query);
      m.addEventListener("change", l);
      return () => m.removeEventListener("change", l);
    },
    () => matchMedia(query).matches,
  );
}
