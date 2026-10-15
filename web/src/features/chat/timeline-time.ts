/**
 * Desktop `formatTimelineTimestamp`: the hover times on user messages and turn
 * footers. The date tier depends on distance from `now`:
 *
 * - under 7 local calendar days: weekday + time
 * - same year: month + day + time
 * - otherwise: year + month + day + time
 *
 * Times are 24-hour; date and time are formatted separately and joined by a space.
 */
const DAY_MS = 24 * 60 * 60 * 1000

function startOfLocalDay(date: Date): number {
  const copy = new Date(date)
  copy.setHours(0, 0, 0, 0)
  return copy.getTime()
}

export function formatTimelineTimestamp(at: Date, locale: string, now = new Date()): string {
  const timePart = new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit", hour12: false }).format(at)
  if (startOfLocalDay(now) - startOfLocalDay(at) < 7 * DAY_MS) {
    return `${new Intl.DateTimeFormat(locale, { weekday: "long" }).format(at)} ${timePart}`
  }
  const datePart = new Intl.DateTimeFormat(locale, {
    ...(at.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
    month: "short",
    day: "numeric",
  }).format(at)
  return `${datePart} ${timePart}`
}

/** Desktop `formatRelativeTimeShort` (sidebar rows): `now`, `3m`, `4h`, `2d`. */
export function formatRelativeTimeShort(value: Date, nowMs: number = Date.now()): string {
  const diff = Math.max(0, nowMs - value.getTime())
  if (diff < 60_000) return "now"
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m`
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h`
  return `${Math.floor(diff / 86_400_000)}d`
}

/** Desktop `formatElapsedSeconds`: `0.4s`, `12s`, `3m`, `3m 5s`. */
export function formatElapsedSeconds(durationMs: number): string {
  const safeDurationMs = Math.max(durationMs, 0)
  const totalSeconds = Math.floor(safeDurationMs / 1000)
  if (totalSeconds < 60) {
    return totalSeconds === 0 && safeDurationMs > 0 ? `${Math.round(safeDurationMs / 100) / 10}s` : `${totalSeconds}s`
  }
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`
}
