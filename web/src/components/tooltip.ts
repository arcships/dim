import type { Directive } from "vue"

/**
 * `v-tooltip="text"`: the control kit's `.ui-tooltip` in place of native `title`.
 * Hover shows it after 400ms (immediately while moving between tooltipped controls,
 * within 300ms of the last one closing), keyboard focus immediately; press, Escape, scroll,
 * leaving or moving the pointer elsewhere hides it. One shared element is shown as a
 * manual popover so it sits in the top layer, above modal dialogs, at fixed coordinates
 * next to its target.
 */
const HOVER_DELAY_MS = 400
const WARM_MS = 300
const GAP = 6
const VIEWPORT_MARGIN = 8
const TOOLTIP_ID = "ui-tooltip"

type Target = HTMLElement & { _tooltip?: string }

let tooltip: HTMLDivElement | undefined
let active: Target | undefined
let timer: ReturnType<typeof setTimeout> | undefined
/** Until this time a hover shows without the delay: the pointer is sweeping a toolbar. */
let warmUntil = 0
/** The element the page returns focus to when the window comes back; that focus is not the user's. */
let focusedAtWindowBlur: Element | null = null
/** Touch browsers emulate mouseenter on tap, and no mouseleave follows to hide it. */
let touchInput = false

function element(): HTMLDivElement {
  if (tooltip !== undefined) return tooltip
  tooltip = document.createElement("div")
  tooltip.id = TOOLTIP_ID
  tooltip.className = "ui-tooltip"
  tooltip.setAttribute("role", "tooltip")
  tooltip.popover = "manual"
  // Reset the UA popover box; .ui-tooltip draws the rest.
  Object.assign(tooltip.style, { position: "fixed", inset: "auto", margin: "0", border: "0", overflow: "visible" })
  document.body.append(tooltip)
  return tooltip
}

function show(target: Target): void {
  clearTimeout(timer)
  const text = target._tooltip
  if (!text || !target.isConnected) return
  const node = element()
  node.textContent = text
  if (!node.matches(":popover-open")) node.showPopover()
  active = target
  // aria-label already names icon buttons; describe only when the tooltip adds text.
  if (target.getAttribute("aria-label") !== text) target.setAttribute("aria-describedby", TOOLTIP_ID)
  place(target, node)
}

function place(target: HTMLElement, node: HTMLElement): void {
  const anchor = target.getBoundingClientRect()
  const box = node.getBoundingClientRect()
  const above = anchor.top - GAP - box.height
  const top = above >= VIEWPORT_MARGIN ? above : anchor.bottom + GAP
  const centered = anchor.left + anchor.width / 2 - box.width / 2
  const left = Math.min(Math.max(centered, VIEWPORT_MARGIN), window.innerWidth - VIEWPORT_MARGIN - box.width)
  node.style.left = `${Math.round(left)}px`
  node.style.top = `${Math.round(top)}px`
}

function hide(): void {
  clearTimeout(timer)
  timer = undefined
  if (active === undefined) return
  if (active.getAttribute("aria-describedby") === TOOLTIP_ID) active.removeAttribute("aria-describedby")
  active = undefined
  if (tooltip?.matches(":popover-open")) tooltip.hidePopover()
}

function onEnter(event: Event): void {
  const target = event.currentTarget as Target
  clearTimeout(timer)
  if (touchInput) return
  if (performance.now() < warmUntil) show(target)
  else timer = setTimeout(() => show(target), HOVER_DELAY_MS)
}

/** Leaving a shown tooltip keeps the next one warm; press, Escape and scroll dismiss cold. */
function onLeave(): void {
  if (active !== undefined) warmUntil = performance.now() + WARM_MS
  hide()
}

function onFocus(event: FocusEvent): void {
  const target = event.currentTarget as Target
  // Coming back to the window refocuses the last element, still :focus-visible after any
  // keypress; showing there would leave a tooltip no pointer movement can dismiss.
  if (target === focusedAtWindowBlur) {
    focusedAtWindowBlur = null
    return
  }
  if (target.matches(":focus-visible")) show(target)
}

/** A focus-shown tooltip sits away from the pointer, so no mouseleave will close it. */
function onPointerMove(event: PointerEvent): void {
  touchInput = event.pointerType === "touch"
  if (active !== undefined && !(event.target instanceof Node && active.contains(event.target))) hide()
}

function onWindowBlur(): void {
  focusedAtWindowBlur = document.activeElement
  hide()
}

function onKey(event: KeyboardEvent): void {
  if (event.key === "Escape") hide()
}

const listeners: [string, EventListener][] = [
  ["mouseenter", onEnter],
  ["mouseleave", onLeave],
  ["focus", onFocus as EventListener],
  ["blur", hide],
  ["pointerdown", hide],
  ["keydown", onKey as EventListener],
]

let globalBound = false
function bindGlobal(): void {
  if (globalBound) return
  globalBound = true
  window.addEventListener("scroll", hide, { capture: true, passive: true })
  window.addEventListener("blur", onWindowBlur)
  document.addEventListener("pointermove", onPointerMove, { passive: true })
  document.addEventListener("pointerdown", (event) => { touchInput = event.pointerType === "touch" }, { capture: true, passive: true })
}

export const vTooltip: Directive<Target, string | undefined> = {
  mounted(target, binding) {
    target._tooltip = binding.value
    for (const [type, listener] of listeners) target.addEventListener(type, listener)
    bindGlobal()
  },
  updated(target, binding) {
    target._tooltip = binding.value
    if (active !== target) return
    if (binding.value) show(target)
    else hide()
  },
  beforeUnmount(target) {
    if (active === target) hide()
    for (const [type, listener] of listeners) target.removeEventListener(type, listener)
  },
}

declare module "vue" {
  interface GlobalDirectives {
    vTooltip: typeof vTooltip
  }
}
