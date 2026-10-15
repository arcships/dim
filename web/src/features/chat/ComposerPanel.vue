<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from "vue"
import { useI18n } from "vue-i18n"
import { ArrowUp, Square } from "lucide-vue-next"
import type { ModelId } from "../../rpc/protocol"
import ComposerModelMenu from "./ComposerModelMenu.vue"

/**
 * The message composer: a growing text field, the model menu, and one button
 * that sends, or stops the run while one is in flight. Enter sends,
 * Shift+Enter breaks the line.
 */
const props = defineProps<{
  running: boolean
  stopping?: boolean
  model: ModelId | undefined
  /** False when this connection may not change the model; the menu then only names it. */
  modelSelectable: boolean
  /** Sends the text; resolves once the server accepted it. */
  send: (text: string) => Promise<void>
  stop?: () => Promise<void>
  selectModel: (model: ModelId) => Promise<void> | void
}>()
const { t } = useI18n()
const field = ref<HTMLTextAreaElement>()
const text = ref("")
const sending = ref(false)
const changingModel = ref(false)
const error = ref("")

const showStop = computed(() => props.running && props.stop !== undefined && text.value.trim() === "")
const canSend = computed(() => text.value.trim() !== "" && !sending.value)

function resize(): void {
  const element = field.value
  if (element === undefined) return
  element.style.height = "auto"
  element.style.height = `${Math.min(element.scrollHeight, 300)}px`
}

async function submit(): Promise<void> {
  if (!canSend.value) return
  const message = text.value
  sending.value = true
  error.value = ""
  try {
    await props.send(message)
    if (text.value === message) text.value = ""
    await nextTick()
    resize()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    sending.value = false
  }
}

async function stop(): Promise<void> {
  error.value = ""
  try {
    await props.stop?.()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  }
}

async function onModel(model: ModelId): Promise<void> {
  changingModel.value = true
  error.value = ""
  try {
    await props.selectModel(model)
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    changingModel.value = false
  }
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key === "Enter" && !event.shiftKey && !event.altKey && !event.isComposing) {
    event.preventDefault()
    void submit()
  }
}

onMounted(() => field.value?.focus())
defineExpose({ focus: () => field.value?.focus() })
</script>

<template>
  <section class="composer-panel" :aria-label="t('chat.composer')">
    <p v-if="error" class="inline-error" role="alert">{{ error }}</p>
    <div class="composer-surface" @click="field?.focus()">
      <textarea
        ref="field"
        v-model="text"
        class="composer-input"
        rows="2"
        :placeholder="t('composer.sendMessage')"
        :aria-label="t('composer.sendMessage')"
        @input="resize"
        @keydown="onKeydown"
      />
      <footer class="composer-toolbar">
        <div class="composer-tool-group" />
        <div class="composer-run-group">
          <ComposerModelMenu :model="model" :selectable="modelSelectable" :disabled="changingModel" @select="onModel" />
          <button
            class="ui-icon-button"
            :data-variant="showStop ? 'contrast' : 'primary'"
            data-size="sm"
            type="button"
            :aria-label="showStop ? t('composer.stopResponse') : t('composer.send')"
            v-tooltip="showStop ? t('composer.stopResponse') : t('composer.send')"
            :disabled="showStop ? stopping : !canSend"
            @click.stop="showStop ? stop() : submit()"
          >
            <Square v-if="showStop" fill="currentColor" aria-hidden="true" />
            <ArrowUp v-else :stroke-width="2.25" aria-hidden="true" />
          </button>
        </div>
      </footer>
    </div>
    <div v-if="$slots.status" class="composer-statusbar">
      <div class="composer-status-start"><slot name="status" /></div>
    </div>
  </section>
</template>

<style scoped>
.composer-input { display: block; width: 100%; min-height: 60px; max-height: 300px; resize: none; padding: 0; border: 0; background: transparent; color: var(--foreground); font: var(--text-base)/var(--leading-relaxed) var(--font-sans); outline: none; }
.composer-input::placeholder { color: var(--muted-foreground); }
</style>
