<script setup lang="ts">
import { watch } from "vue"
import { useToastStore, type Toast } from "../stores/toast"

const toastStore = useToastStore()
const uiToast = useToast()
const active = new Map<number, string>()

function addToast(item: Toast): void {
  const color = item.tone === "danger" ? "error" : item.tone === "info" ? "info" : "success"
  const icon = item.tone === "danger" ? "i-lucide-x" : item.tone === "info" ? "i-lucide-info" : "i-lucide-check"
  const id = String(item.id)
  active.set(item.id, id)
  uiToast.add({
    id,
    title: item.title,
    description: item.description,
    color,
    icon,
    duration: 4000,
    actions: item.action
      ? [{
          label: item.action.label,
          variant: "ghost",
          color: "primary",
          onClick: () => {
            toastStore.dismiss(item.id)
            item.action?.run()
          },
        }]
      : undefined,
    "onUpdate:open": (open: boolean) => {
      if (!open) toastStore.dismiss(item.id)
    },
  })
}

watch(() => toastStore.toasts, (items) => {
  const current = new Set(items.map((item) => item.id))
  for (const [storeId, uiId] of active) {
    if (!current.has(storeId)) {
      active.delete(storeId)
      uiToast.remove(uiId)
    }
  }
  for (const item of items) {
    if (!active.has(item.id)) addToast(item)
  }
}, { immediate: true })
</script>

<template />
