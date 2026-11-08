/**
 * The words a failed or retrying model request uses in the chat, following the
 * CLI's `crates/dim-cli/src/projection.rs` and `display.rs`: the kind supply the
 * meaning and the action, the provider's reason follows the meaning, and the
 * client adds no prefix of its own.
 */
import type { ModelFailure, ModelRetryProgress } from "../../npm/client-protocol"
import { translate, type TranslationKey } from "./i18n/index"

/** Every kind the wire can carry; the tables must have a row for each. */
export const FAILURE_KINDS = [
  "network",
  "rate_limited",
  "server_error",
  "malformed_response",
  "auth_rejected",
  "quota_exhausted",
  "not_found",
  "context_too_long",
  "content_filtered",
  "invalid_request",
  "output_limit",
  "unknown",
] as const

function meaningKey(kind: string): TranslationKey {
  return `failure.kind.${kind}.meaning` as TranslationKey
}

function actionKey(kind: string): TranslationKey {
  return `failure.kind.${kind}.action` as TranslationKey
}

/** A reason without its trailing period, which the line supplies itself. */
function bare(reason: string | undefined): string | undefined {
  const trimmed = reason?.trim()
  if (trimmed === undefined || trimmed === "") return undefined
  return trimmed.replace(/\.$/u, "")
}

/** `<meaning>: <reason>. <action>`, after the incomplete sentence once output showed. */
export function failureLine(failure: ModelFailure, incomplete: boolean): string {
  const reason = bare(failure.reason)
  const meaning = translate(meaningKey(failure.kind))
  const core = reason === undefined ? meaning : `${meaning}: ${reason}.`
  const prefix = incomplete ? `${translate("failure.incomplete")} ` : ""
  const attempts = failure.attempts > 1
    ? ` ${translate("failure.tried", { count: failure.attempts, seconds: Math.floor((failure.waitedMs + 500) / 1000) })}`
    : ""
  return `${prefix}${core} ${translate(actionKey(failure.kind))}${attempts}`
}

/** The detail lines behind one error row, one fact per line. */
export function failureDetails(failure: ModelFailure): string[] {
  const lines = [translate("failure.details.kind", { kind: failure.kind })]
  if (failure.status !== undefined) lines.push(translate("failure.details.status", { status: failure.status }))
  if (failure.providerCode !== undefined) lines.push(translate("failure.details.providerCode", { code: failure.providerCode }))
  if (failure.requestId !== undefined) lines.push(translate("failure.details.requestId", { id: failure.requestId }))
  if (failure.attempts > 1) {
    lines.push(translate("failure.details.tried", { count: failure.attempts, seconds: (failure.waitedMs / 1000).toFixed(1) }))
  }
  if (failure.retryAfterMs !== undefined) {
    lines.push(translate("failure.details.retryAfter", { seconds: Math.floor(failure.retryAfterMs / 1000) }))
  }
  if (failure.cause.length > 0) {
    lines.push(translate("failure.details.cause"))
    lines.push(...failure.cause)
  }
  return lines
}

/** Whole seconds until a retry starts, never below zero. */
export function retrySeconds(retryAtMs: number, now: number): number {
  return Math.max(0, Math.ceil((retryAtMs - now) / 1000))
}

/** `Retrying 3/8 in 4 s · <meaning>: <reason> · <stop>`, with the client's stop wording. */
export function retryStatus(retry: ModelRetryProgress, seconds: number): string {
  const reason = bare(retry.reason)
  const meaning = translate(meaningKey(retry.kind))
  const wording = reason === undefined ? meaning : `${meaning}: ${reason}`
  const stop = translate("composer.stopResponse")
  if (seconds <= 0) return translate("work.waiting", { reason: wording, stop })
  return translate("work.retrying", {
    attempt: retry.attempt,
    max: retry.maxAttempts,
    seconds,
    reason: wording,
    stop,
  })
}

/**
 * What a screen reader hears while a retry waits: it changes only when the
 * attempt changes or the retry clears, never with the visible countdown.
 */
export function retryAnnouncement(retry: ModelRetryProgress | undefined): string {
  if (retry === undefined) return ""
  return translate("failure.retryAnnouncement", {
    attempt: retry.attempt,
    max: retry.maxAttempts,
    reason: translate(meaningKey(retry.kind)),
  })
}
