<script lang="ts">
/** Icons the shared timeline's TypeScript renderers name. The Nuxt UI icon scan reads `.vue` files only, so the names live here. */
export const ICON = {
  appWindow: "i-lucide-app-window",
  bot: "i-lucide-bot",
  check: "i-lucide-check",
  chevronDown: "i-lucide-chevron-down",
  chevronRight: "i-lucide-chevron-right",
  chevronUp: "i-lucide-chevron-up",
  circle: "i-lucide-circle",
  circleCheck: "i-lucide-circle-check",
  copy: "i-lucide-copy",
  externalLink: "i-lucide-external-link",
  filePen: "i-lucide-file-pen",
  filePlus: "i-lucide-file-plus",
  fileSearch: "i-lucide-file-search",
  fileText: "i-lucide-file-text",
  folder: "i-lucide-folder",
  gitBranch: "i-lucide-git-branch",
  gitFork: "i-lucide-git-fork",
  globe: "i-lucide-globe",
  hourglass: "i-lucide-hourglass",
  layers: "i-lucide-layers",
  listChecks: "i-lucide-list-checks",
  listTodo: "i-lucide-list-todo",
  loaderCircle: "i-lucide-loader-circle",
  messageCircleQuestion: "i-lucide-message-circle-question",
  mousePointerClick: "i-lucide-mouse-pointer-click",
  octagonPause: "i-lucide-octagon-pause",
  plug: "i-lucide-plug",
  rotateCcw: "i-lucide-rotate-ccw",
  search: "i-lucide-search",
  sparkles: "i-lucide-sparkles",
  terminal: "i-lucide-terminal",
  triangleAlert: "i-lucide-triangle-alert",
  workflow: "i-lucide-workflow",
  wrench: "i-lucide-wrench",
} as const
</script>

<script setup lang="ts">
defineProps<{ name: string }>()
</script>

<template>
  <UIcon :name="name" aria-hidden="true" />
</template>
