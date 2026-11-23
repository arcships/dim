export type * from "../../../npm/client-protocol"

export const PROTOCOL_VERSION = 4
/** The root line's id: the line of a Session that has no parent. */
export const ROOT_BRANCH_ID = "main"

export type Delegation = {
  id: string
  title: string
  prompt: string
  at: number
  state: "starting" | "running" | "completed" | "failed" | "cancelled"
  memberId?: string
  agentId?: string
  result?: string
  reason?: string
}
