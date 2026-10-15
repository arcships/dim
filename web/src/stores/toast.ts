import { ref } from "vue"
import { defineStore } from "pinia"

export type ToastTone = "success" | "info" | "danger"

export type ToastInput = {
  tone: ToastTone
  title: string
  description?: string
  /** One trailing ghost button ("Undo"); running it also dismisses the toast. */
  action?: { label: string; run: () => unknown }
}

export type Toast = ToastInput & { id: number }

const DISMISS_AFTER_MS = 4000
const MAX_VISIBLE = 3

/** Transient feedback for finished background actions, shown bottom-right by ToastRegion. */
export const useToastStore = defineStore("toast", () => {
  const toasts = ref<Toast[]>([])
  const timers = new Map<number, ReturnType<typeof setTimeout>>()
  let seq = 0

  function dismiss(id: number): void {
    clearTimeout(timers.get(id))
    timers.delete(id)
    toasts.value = toasts.value.filter((toast) => toast.id !== id)
  }

  /** (Re)starts the auto-dismiss countdown; hovering a toast pauses it. */
  function resume(id: number): void {
    clearTimeout(timers.get(id))
    timers.set(id, setTimeout(() => dismiss(id), DISMISS_AFTER_MS))
  }

  function pause(id: number): void {
    clearTimeout(timers.get(id))
    timers.delete(id)
  }

  function push(input: ToastInput): void {
    const id = ++seq
    const next = [...toasts.value, { ...input, id }]
    for (const dropped of next.slice(0, -MAX_VISIBLE)) pause(dropped.id)
    toasts.value = next.slice(-MAX_VISIBLE)
    resume(id)
  }

  return { toasts, push, dismiss, pause, resume }
})
