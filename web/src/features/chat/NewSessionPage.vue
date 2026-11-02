<script setup lang="ts">
import { computed, ref, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import { Check, ChevronDown, Folder } from "lucide-vue-next"
import BrandMark from "../../components/BrandMark.vue"
import ComposerPanel from "./ComposerPanel.vue"
import { MAIN_BRANCH, type ModelId } from "../../rpc/protocol"
import { useChatStore } from "../../stores/chat"
import { useModelsStore } from "../../stores/models"
import { useSessionsStore, workspaceName } from "../../stores/sessions"

/**
 * A new session: pick a known workspace or type an absolute directory, write
 * the first message, and send. Sending creates the session there, submits the
 * message and opens it.
 */
const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const sessions = useSessionsStore()
const chat = useChatStore()
const models = useModelsStore()
const menu = ref<HTMLDetailsElement>()

/** The directory the session will run in, seeded from `?cwd=` or the latest workspace. */
const cwd = ref(typeof route.query.cwd === "string" ? route.query.cwd : "")
const typed = ref("")
watch(() => sessions.workspaces, (workspaces) => {
  if (cwd.value === "" && workspaces[0] !== undefined) cwd.value = workspaces[0].cwd
}, { immediate: true })

/** The model picked here; until then, the directory's default. */
const picked = ref<ModelId>()
const fallback = ref<ModelId>()
watch(cwd, async (dir) => {
  fallback.value = undefined
  if (dir === "") return
  const model = await models.defaultFor(dir)
  if (cwd.value === dir) fallback.value = model
}, { immediate: true })
const model = computed(() => picked.value ?? fallback.value)

function choose(dir: string): void {
  cwd.value = dir
  if (menu.value) menu.value.open = false
}

function useTyped(): void {
  const dir = typed.value.trim()
  if (dir === "") return
  choose(dir)
  typed.value = ""
}

async function send(text: string): Promise<void> {
  if (cwd.value === "") throw new Error(t("newThread.workspaceRequired"))
  const sessionId = await sessions.create(cwd.value, picked.value)
  chat.open(sessionId)
  await chat.submit(sessionId, MAIN_BRANCH, text)
  await router.push({ name: "chat-session", params: { sessionId } })
}
</script>

<template>
  <section class="new-session-page">
    <div class="new-session-hero">
      <BrandMark class="new-session-mark" />
      <h1 data-route-heading tabindex="-1">{{ t("newThread.title") }}</h1>
    </div>
    <ComposerPanel class="is-new-thread" :running="false" :model="model" :model-selectable="true" :send="send" :attachments="false" :select-model="(next) => { picked = next }">
      <template #status>
        <details ref="menu" class="composer-popover workspace-menu">
          <summary class="ui-button" data-variant="ghost" data-size="xs" data-trailing v-tooltip="cwd || undefined">
            <Folder aria-hidden="true" />
            <span class="workspace-trigger-label">{{ cwd ? workspaceName(cwd) : t("newThread.selectWorkspace") }}</span>
            <ChevronDown class="workspace-chevron" aria-hidden="true" />
          </summary>
          <div class="composer-popover-content workspace-menu-content ui-popover" data-width="lg">
            <form class="workspace-path-form" @submit.prevent="useTyped">
              <input v-model="typed" class="ui-field" data-size="sm" type="text" spellcheck="false" autocapitalize="off" :placeholder="t('newThread.pathPlaceholder')" :aria-label="t('newThread.pathLabel')">
              <button type="submit" class="ui-button" data-variant="secondary" data-size="sm" :disabled="typed.trim() === ''">{{ t("newThread.usePath") }}</button>
            </form>
            <hr v-if="sessions.workspaces.length" class="ui-menu-separator">
            <div class="workspace-list" role="listbox" :aria-label="t('sidebar.workspaces')">
              <button
                v-for="workspace in sessions.workspaces"
                :key="workspace.cwd"
                type="button"
                role="option"
                class="ui-menu-item"
                data-two-line
                :aria-selected="workspace.cwd === cwd"
                @click="choose(workspace.cwd)"
              >
                <Folder aria-hidden="true" />
                <span>{{ workspace.name }}<span class="ui-menu-desc workspace-path">{{ workspace.cwd }}</span></span>
                <Check v-if="workspace.cwd === cwd" class="ui-menu-check" aria-hidden="true" />
              </button>
            </div>
          </div>
        </details>
      </template>
    </ComposerPanel>
  </section>
</template>

<style scoped>
.workspace-trigger-label { max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.workspace-chevron { transition: transform var(--duration-press) var(--ease-out); }
.workspace-menu[open] .workspace-chevron { transform: rotate(180deg); }
.workspace-menu-content { display: flex; flex-direction: column; }
.workspace-path-form { display: flex; gap: var(--space-1); padding: var(--space-1); }
.workspace-path-form > .ui-field { min-width: 0; flex: 1; font-family: var(--font-mono); }
.workspace-list { min-height: 0; max-height: 240px; overflow-x: hidden; overflow-y: auto; }
.workspace-path { overflow: hidden; font-family: var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
</style>
