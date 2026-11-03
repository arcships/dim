<script setup lang="ts">
import { ref } from "vue"
import { useI18n } from "vue-i18n"
import type { QueuedInput } from "../../rpc/protocol"
import { useChatStore } from "../../stores/chat"
import { useToastStore } from "../../stores/toast"

/**
 * The messages queued while the main line runs, above the composer. Steer
 * sends the oldest one into the run at its next turn boundary; Remove drops
 * any of them. Both withdraw the message through `session.input.withdraw`.
 */
const props = defineProps<{ sessionId: string; items: readonly QueuedInput[] }>()
const { t } = useI18n()
const chat = useChatStore()
const toast = useToastStore()
const busy = ref(false)

async function withdraw(item: QueuedInput, steer: boolean): Promise<void> {
  if (busy.value) return
  busy.value = true
  try {
    const result = await chat.withdrawInput(props.sessionId, item.id, steer ? item.user : undefined)
    if (result.status === "notHeld") toast.push({ tone: "info", title: t("queue.notHeld") })
  } catch (cause) {
    toast.push({ tone: "danger", title: t("command.failed"), description: cause instanceof Error ? cause.message : String(cause) })
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <section class="queued-inputs" :aria-label="t('queue.title')">
    <h2 class="queued-title">{{ t("queue.title") }}</h2>
    <ol class="queued-list">
      <li v-for="(item, index) in items" :key="item.id" class="queued-item">
        <span class="queued-index">{{ index + 1 }}</span>
        <span class="queued-text">{{ item.text }}</span>
        <span v-for="skill in item.skills" :key="skill" class="queued-skill">{{ t("message.skillLoaded", { name: skill }) }}</span>
        <button v-if="index === 0" type="button" class="ui-button" data-size="sm" data-variant="secondary" :disabled="busy" @click="withdraw(item, true)">{{ t("queue.steer") }}</button>
        <button type="button" class="ui-button" data-size="sm" data-variant="ghost" :disabled="busy" @click="withdraw(item, false)">{{ t("queue.remove") }}</button>
      </li>
    </ol>
  </section>
</template>

<style scoped>
.queued-inputs { display: grid; gap: var(--space-1); padding: var(--space-2) var(--space-3); border: 1px dashed var(--border); border-radius: var(--radius-lg); background: var(--muted); font-size: var(--text-sm); }
.queued-title { margin: 0; color: var(--muted-foreground); font-size: var(--text-xs); font-weight: var(--weight-medium); }
.queued-list { display: grid; gap: var(--space-1); margin: 0; padding: 0; list-style: none; }
.queued-item { display: flex; min-width: 0; align-items: center; gap: var(--space-2); }
.queued-index { color: var(--muted-foreground); font-variant-numeric: tabular-nums; }
.queued-text { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.queued-skill { color: var(--muted-foreground); font-size: var(--text-xs); white-space: nowrap; }
</style>
