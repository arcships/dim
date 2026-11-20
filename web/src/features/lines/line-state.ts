import type { LineState, LineView } from "../../lines"
import { ROOT_BRANCH_ID, type ApprovalDecision } from "../../rpc/protocol"

export type Translate = (key: string, params?: Record<string, unknown>) => string

export const DETAIL_COVER_WIDTH = 1100

export type ApprovalLayout = {
  detailOpen: boolean
  detailExpanded: boolean
  shellWidth: number
  /** The line the Session's conversation is on: the centre's own prompt is the main one. */
  defaultBranchId: string
  activeLine?: string | undefined
  detailFocused?: boolean
}

export type ApprovalKeyEvent = {
  key: string
  defaultPrevented?: boolean
  target?: unknown
}

export function shortId(id: string): string {
  return id.slice(0, 8)
}

export function lineStateLabel(t: Translate, state: LineState): string {
  switch (state.kind) {
    case ROOT_BRANCH_ID: return t("lines.state.main")
    case "working": return t("lines.state.working")
    case "waiting": return t("lines.state.waiting")
    case "ending": return t("lines.state.ending")
    case "ended": return t("lines.state.ended")
    case "ended-itself": return t("lines.state.endedItself")
    case "ended-by-operation": return t("lines.state.endedByOperation")
    case "ended-with": return t("lines.state.endedWith", { id: state.ancestor })
    case "creation-failed": return t("lines.state.creationFailed")
  }
}

export function lineStateTone(state: LineState): "primary" | "warning" | "danger" | undefined {
  if (state.kind === "working") return "primary"
  if (state.kind === "creation-failed") return "danger"
  if (state.kind === "ended-with") return "warning"
  return undefined
}

export function lineName(t: Translate, line: LineView | undefined, branchId: string): string {
  const purpose = line?.purpose.trim() ?? ""
  return purpose === "" ? shortId(branchId) : purpose
}

/** The marks a line's row carries: the role the default branch holds, and where the Session started (design §14). */
export function lineMarks(id: string, defaultBranchId: string): Array<"main-line" | "start"> {
  const marks: Array<"main-line" | "start"> = []
  if (id === defaultBranchId) marks.push("main-line")
  if (id === ROOT_BRANCH_ID) marks.push("start")
  return marks
}

/**
 * Whether the tree offers "continue from here" on a line. The method decides
 * every refusal; a line already holding the role is not offered a move to
 * itself, and a line that never opened has nothing to continue on.
 */
export function canContinueFrom(line: { id: string; state: LineState }, defaultBranchId: string): boolean {
  return line.id !== defaultBranchId && line.state.kind !== "creation-failed"
}

/**
 * The branch whose visible approval prompt owns the window keyboard shortcuts
 * (`Enter`/`y` allow, `Escape`/`n` deny), or `undefined` when no visible
 * surface has a pending approval.
 */
export function approvalKeyboardBranch(
  approvals: readonly { branchId: string }[],
  layout: ApprovalLayout,
): string | undefined {
  const mainVisible = !layout.detailOpen || (!layout.detailExpanded && layout.shellWidth >= DETAIL_COVER_WIDTH)
  const activeLine = layout.detailOpen ? layout.activeLine : undefined
  const mainHas = mainVisible && approvals.some((request) => request.branchId === layout.defaultBranchId)
  const lineHas = activeLine !== undefined && approvals.some((request) => request.branchId === activeLine)
  if (mainHas && lineHas) return layout.detailFocused ? activeLine : layout.defaultBranchId
  if (lineHas) return activeLine
  if (mainHas) return layout.defaultBranchId
  return undefined
}

export function approvalKeyDecision(
  event: ApprovalKeyEvent,
  options: { keyboard?: boolean; pending?: boolean; dialogOpen?: boolean } = {},
): ApprovalDecision | undefined {
  if (options.keyboard === false || options.pending || event.defaultPrevented || options.dialogOpen) return undefined
  const target = event.target as { tagName?: unknown; isContentEditable?: unknown } | null | undefined
  if (target !== null && typeof target === "object") {
    if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable === true) return undefined
  }
  if (event.key === "Enter" || event.key === "y" || event.key === "Y") return "allow"
  if (event.key === "Escape" || event.key === "n" || event.key === "N") return "deny"
  return undefined
}
