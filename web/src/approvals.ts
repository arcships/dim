/**
 * Which extra answers a tool approval offers, following the TUI's
 * `crates/dim-cli/src/permission.rs` (`always_command`) and `ui/prompts.rs`
 * (`grant_for`). The App Server enforces every rule and grant; these checks
 * only decide what the prompt shows.
 */
import type { ApprovalRequest } from "./rpc/protocol"
import type { CommandRule } from "./rpc/general-settings"

/** Characters that make a command more than one simple command. */
const COMMAND_SEPARATORS = /[;&|`()<>$\r\n]/

/** The simple command an "always allow" answer may save, or `undefined`. */
export function alwaysCommand(request: ApprovalRequest): string | undefined {
  if (request.call.name !== "exec" && request.call.name !== "execute") return undefined
  const command = request.call.input.command
  if (typeof command !== "string" || COMMAND_SEPARATORS.test(command) || command.trim() === "") return undefined
  return command
}

/** The exact allow rule for a simple command: its words, one space apart. */
export function alwaysRule(command: string): CommandRule {
  return { command: command.split(/\s+/).filter((word) => word !== "").join(" "), match: "exact", decision: "allow" }
}

/** Whether `existing` already allows exactly what `rule` allows. */
export function sameRule(existing: CommandRule, rule: CommandRule): boolean {
  return existing.decision === rule.decision && existing.match === rule.match && existing.command.split(/\s+/).filter((word) => word !== "").join(" ") === rule.command
}

/** The session grant a file tool's approval may add: its path, read or write. */
export function fileGrant(request: ApprovalRequest): { path: string; kind: "file"; mode: "read" | "write" } | undefined {
  const tool = request.call.name
  if (tool !== "read" && tool !== "write" && tool !== "edit") return undefined
  const path = request.call.input.path
  if (typeof path !== "string") return undefined
  return { path, kind: "file", mode: tool === "read" ? "read" : "write" }
}
