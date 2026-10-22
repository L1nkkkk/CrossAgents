// Mock data for the start page: an empty hub, as on first launch (preview:mock).

import { emptyState, type MockState } from "../dev/MockBridge";
import { plugins } from "./Agents.mock";

/** Nothing yet: no teammates, no runs, no accounts. The start page shows its three steps. */
export function emptyHub(): MockState {
  return { ...emptyState(), plugins: plugins() };
}
