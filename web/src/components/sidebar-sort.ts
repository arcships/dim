import type { SessionSummary } from "../rpc/protocol"
import type { SidebarSortBy } from "../stores/ui"

/** Recent activity sorts by the last main-branch event; recent creation sorts by when the session began. */
export function sortKey(session: SessionSummary, sortBy: SidebarSortBy): number {
  return sortBy === "recentCreated" ? session.createdAt : session.updatedAt
}
