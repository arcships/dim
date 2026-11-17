<script setup lang="ts">
defineProps<{ state: string; active: boolean }>()
</script>

<template>
  <span class="work-status-icon" :data-state="state" :data-active="active" aria-hidden="true">
    <svg v-if="state === 'thinking'" viewBox="0 0 24 24" fill="currentColor">
      <path class="status-star" d="M9 4 11.5 10.5 18 13 11.5 15.5 9 22 6.5 15.5 0 13 6.5 10.5Z" />
      <path class="status-twinkle" d="m19 1 1.3 3.7L24 6l-3.7 1.3L19 11l-1.3-3.7L14 6l3.7-1.3Z" />
    </svg>
    <svg v-else-if="state === 'command'" viewBox="0 0 24 24" stroke="currentColor" stroke-width="3" stroke-linecap="round">
      <path class="status-bar" d="M5 8v12" />
      <path class="status-bar" d="M12 4v16" />
      <path class="status-bar" d="M19 10v10" />
    </svg>
    <UIcon v-else :name="state === 'error' ? 'i-lucide-x' : state === 'done' ? 'i-lucide-check' : state === 'edit' ? 'i-lucide-pencil' : state === 'search' || state === 'web_search' || state === 'web_fetch' ? 'i-lucide-search' : state === 'read' ? 'i-lucide-file-text' : 'i-lucide-wrench'" />
  </span>
</template>

<style scoped>
.work-status-icon { display: inline-flex; width: 20px; height: 20px; flex: none; align-items: center; justify-content: center; color: var(--muted-foreground); }
.work-status-icon > * { width: 18px; height: 18px; }
.work-status-icon[data-state="error"] { color: var(--danger); }
[data-active="true"] .status-star { transform-origin: 9px 13px; animation: star-turn 2.4s ease-in-out infinite; }
[data-active="true"] .status-twinkle { transform-origin: 19px 6px; animation: star-glow 1.2s ease-in-out infinite; }
[data-active="true"] .status-bar { transform-origin: center bottom; animation: bar-wave .9s ease-in-out infinite alternate; }
.status-bar:nth-child(2) { animation-delay: -.3s; }
.status-bar:nth-child(3) { animation-delay: -.6s; }
[data-active="true"]:is([data-state="search"], [data-state="web_search"], [data-state="web_fetch"]) > * { animation: search-orbit 2s linear infinite; }
[data-active="true"][data-state="edit"] > * { animation: pen-write 1.2s ease-in-out infinite; }
@keyframes star-turn { 50% { transform: rotate(45deg) scale(.8); } 100% { transform: rotate(90deg); } }
@keyframes star-glow { 50% { transform: scale(.5); opacity: .3; } }
@keyframes bar-wave { to { transform: scaleY(.35); } }
@keyframes search-orbit { 0%, 100% { transform: translateY(-2px); } 25% { transform: translateX(2px); } 50% { transform: translateY(2px); } 75% { transform: translateX(-2px); } }
@keyframes pen-write { 25% { transform: translate(2px, -1px) rotate(-8deg); } 75% { transform: translate(-2px, 1px) rotate(8deg); } }
@media (prefers-reduced-motion: reduce) { .work-status-icon * { animation: none !important; } }
</style>
