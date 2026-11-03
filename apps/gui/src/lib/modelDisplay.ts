import type { AccountQuota } from "@bindings/AccountQuota";
import { prettyModel } from "./models.ts";

/** CLI names resolve aliases; only use metadata from the chosen provider and login. */
export function modelDisplay(plugin: string, model: string, accounts: readonly AccountQuota[], account?: string) {
  const matches = accounts.filter((a) => a.account.provider === plugin && (!account || a.account.id === account))
    .flatMap((a) => a.models.filter((m) => m.id === model));
  const info = matches[0];
  const unambiguous = info && matches.every((m) => m.name === info.name);
  return {
    label: unambiguous && info.name.trim() ? info.name : prettyModel(model),
    description: unambiguous ? info.description : undefined,
  };
}
