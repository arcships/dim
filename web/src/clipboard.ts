/**
 * Copy text to the system clipboard. `navigator.clipboard` exists only in secure
 * contexts, and the web host is often opened over plain http on a LAN or
 * Tailscale address; there the legacy `execCommand("copy")` path still works,
 * as long as this is called synchronously from the user's click.
 */
export async function copyText(text: string): Promise<void> {
  if (window.isSecureContext) return navigator.clipboard.writeText(text)
  const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null
  const field = document.createElement("textarea")
  field.value = text
  field.setAttribute("readonly", "")
  // 16px keeps iOS Safari from zooming in on focus; off-screen keeps it invisible.
  field.style.cssText = "position:fixed;top:0;left:-9999px;font-size:16px;opacity:0"
  document.body.append(field)
  field.select()
  field.setSelectionRange(0, text.length)
  const copied = document.execCommand("copy")
  field.remove()
  previousFocus?.focus({ preventScroll: true })
  if (!copied) throw new Error("Copy to clipboard failed")
}
