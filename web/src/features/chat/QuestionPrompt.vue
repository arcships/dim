<script setup lang="ts">
import { computed, reactive, ref } from "vue"
import { useI18n } from "vue-i18n"
import { CircleHelp } from "lucide-vue-next"
import type { AskQuestion, PendingQuestion, QuestionAnswer } from "../../rpc/protocol"
import { RpcError } from "../../rpc/client"
import { useChatStore } from "../../stores/chat"

/**
 * The `ask_user` prompt: it takes the composer's place and asks the questions
 * of one waiting call. "Send answers" answers the call, "Decline" answers it
 * with no answers. The prompt closes when the batch's result reaches the
 * timeline, so a rejected or late answer keeps the form for another try.
 */
const props = defineProps<{ sessionId: string; question: PendingQuestion }>()
const { t } = useI18n()
const chat = useChatStore()
const busy = ref(false)
const sent = ref(false)
const error = ref("")
const selected = reactive<Record<string, string[]>>({})
const typed = reactive<Record<string, string>>({})
const radioGroup = (question: AskQuestion) => `question-${props.question.callId}-${question.id}`

function typedAnswer(question: AskQuestion): string | undefined {
  const text = (typed[question.id] ?? "").trim()
  return question.allowCustom && text !== "" ? text : undefined
}

const complete = computed(() => props.question.questions.every((question) => (selected[question.id]?.length ?? 0) > 0 || typedAnswer(question) !== undefined))

function choose(question: AskQuestion, label: string): void {
  const current = selected[question.id] ?? []
  if (!question.multiSelect) {
    selected[question.id] = [label]
    typed[question.id] = ""
  } else selected[question.id] = current.includes(label) ? current.filter((item) => item !== label) : [...current, label]
}

function typeAnswer(question: AskQuestion, text: string): void {
  typed[question.id] = text
  if (!question.multiSelect && text.trim() !== "") selected[question.id] = []
}

function answers(): QuestionAnswer[] {
  return props.question.questions.map((question) => {
    const custom = typedAnswer(question)
    return { questionId: question.id, selected: selected[question.id] ?? [], ...(custom === undefined ? {} : { custom }) }
  })
}

async function send(decline: boolean): Promise<void> {
  if (busy.value || sent.value || (!decline && !complete.value)) return
  busy.value = true
  error.value = ""
  try {
    await chat.answerQuestion(props.sessionId, props.question, decline ? undefined : answers())
    sent.value = true
  } catch (cause) {
    error.value = cause instanceof RpcError && cause.appError === "question_not_pending" ? t("question.notPending") : cause instanceof Error && cause.message ? cause.message : t("question.failed")
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <article class="prompt-card" data-inline-interaction-prompt>
    <header class="prompt-header">
      <span class="prompt-icon" aria-hidden="true"><CircleHelp :size="12" :stroke-width="2.25" /></span>
      <h2 class="prompt-title">{{ t("question.title") }}</h2>
      <span class="ui-badge" data-tone="warning">{{ sent ? t("question.sent") : t("question.awaitingAnswer") }}</span>
    </header>

    <form class="question-list" @submit.prevent="send(false)">
      <fieldset v-for="question in props.question.questions" :key="question.id" class="question-item" :disabled="busy || sent">
        <legend>
          <span v-if="question.header" class="question-header">{{ question.header }}</span>
          {{ question.question }}
          <small v-if="question.multiSelect" class="question-hint">{{ t("question.selectMany") }}</small>
        </legend>
        <label v-for="option in question.options" :key="option.label" class="question-option">
          <input
            :type="question.multiSelect ? 'checkbox' : 'radio'"
            :name="radioGroup(question)"
            :checked="(selected[question.id] ?? []).includes(option.label)"
            @change="choose(question, option.label)"
          >
          <span class="question-option-text">
            <span>{{ option.label }}</span>
            <small v-if="option.description" class="question-option-description">{{ option.description }}</small>
          </span>
        </label>
        <input
          v-if="question.allowCustom"
          class="ui-field question-other"
          data-size="md"
          type="text"
          :value="typed[question.id] ?? ''"
          :placeholder="t('question.otherPlaceholder')"
          :aria-label="t('question.other')"
          @input="typeAnswer(question, ($event.target as HTMLInputElement).value)"
        >
      </fieldset>
    </form>

    <p v-if="error" class="prompt-error" role="alert">{{ error }}</p>
    <div class="prompt-buttons">
      <button type="button" class="ui-button" data-size="md" data-variant="ghost" :disabled="busy || sent" @click="send(true)">{{ t("question.decline") }}</button>
      <button type="button" class="ui-button" data-size="md" data-variant="primary" :disabled="busy || sent || !complete" @click="send(false)">{{ t("question.submit") }}</button>
    </div>
  </article>
</template>

<style scoped>
.question-list { display: grid; gap: var(--space-3); margin-top: var(--space-3); }
.question-item { min-width: 0; margin: 0; padding: 0; border: 0; }
.question-item legend { margin-bottom: var(--space-2); font-size: var(--text-sm); font-weight: var(--weight-medium); }
.question-header { margin-right: var(--space-2); color: var(--muted-foreground); }
.question-hint { margin-left: var(--space-2); color: var(--muted-foreground); font-weight: var(--weight-regular); }
.question-option { display: flex; align-items: flex-start; gap: var(--space-2); padding: var(--space-1) 0; font-size: var(--text-sm); cursor: pointer; }
.question-option-text { display: grid; min-width: 0; gap: 2px; }
.question-option-description { color: var(--muted-foreground); }
.question-other { margin-top: var(--space-2); width: 100%; }
</style>
