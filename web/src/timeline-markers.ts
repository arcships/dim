/**
 * The markers of the timeline rail: one per user message, with the first reply
 * after it, so the rail can jump to a message and preview it. A port of dim-agent's
 * user-message rail (`timeline-user-message-rail.ts`); the rail appears from
 * three user messages on, as it does there.
 */
import type { DisplayRow } from "./features/chat/work"

export const RAIL_MIN_MARKERS = 3
const PREVIEW_CHARS = 180

export type TimelineMarker = {
  key: string
  /** The message's index in the display list the virtualizer scrolls. */
  index: number
  prompt: string
  reply?: string
}

export function previewText(text: string): string {
  return text.replace(/\s+/g, " ").trim().slice(0, PREVIEW_CHARS)
}

export function timelineMarkers(display: readonly DisplayRow[]): TimelineMarker[] {
  const markers: TimelineMarker[] = []
  display.forEach((entry, index) => {
    if (entry.kind !== "row") return
    const row = entry.row
    if (row.kind === "user") {
      markers.push({ key: entry.key, index, prompt: previewText(row.text) })
      return
    }
    const last = markers.at(-1)
    if (row.kind === "assistant" && last !== undefined && last.reply === undefined && row.text.trim() !== "") last.reply = previewText(row.text)
  })
  return markers
}
