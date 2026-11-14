<script setup lang="ts">
import { computed, ref, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import { useI18n } from "vue-i18n"
import BrandMark from "../../components/BrandMark.vue"
import ComposerPanel from "./ComposerPanel.vue"
import ExtensionMenu from "../extensions/ExtensionMenu.vue"
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
const menuOpen = ref(false)

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
  menuOpen.value = false
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
      <ExtensionMenu />
    </div>
    <ComposerPanel class="is-new-thread" :running="false" :model="model" :model-selectable="true" :send="send" :attachments="false" :select-model="(next) => { picked = next }">
      <template #status>
        <UPopover v-model:open="menuOpen" :content="{ side: 'top', align: 'start' }">
          <UTooltip :text="cwd || undefined" :disabled="!cwd">
            <UButton
              color="neutral"
              variant="ghost"
              size="xs"
              icon="i-lucide-folder"
              trailing-icon="i-lucide-chevron-down"
              :ui="{ trailingIcon: menuOpen ? 'rotate-180 transition-transform duration-150' : 'transition-transform duration-150' }"
            >
              <span class="workspace-trigger-label">{{ cwd ? workspaceName(cwd) : t("newThread.selectWorkspace") }}</span>
            </UButton>
          </UTooltip>
          <template #content>
            <div class="workspace-menu-content">
              <form class="workspace-path-form" @submit.prevent="useTyped">
                <UInput
                  v-model="typed"
                  size="sm"
                  class="workspace-path-input"
                  :ui="{ base: 'font-mono' }"
                  spellcheck="false"
                  autocapitalize="off"
                  :placeholder="t('newThread.pathPlaceholder')"
                  :aria-label="t('newThread.pathLabel')"
                />
                <UButton type="submit" color="neutral" variant="soft" size="sm" :disabled="typed.trim() === ''">{{ t("newThread.usePath") }}</UButton>
              </form>
              <hr v-if="sessions.workspaces.length" class="workspace-separator">
              <div class="workspace-list" role="listbox" :aria-label="t('sidebar.workspaces')">
                <UButton
                  v-for="workspace in sessions.workspaces"
                  :key="workspace.cwd"
                  color="neutral"
                  variant="ghost"
                  size="sm"
                  block
                  role="option"
                  class="workspace-option"
                  :aria-selected="workspace.cwd === cwd"
                  @click="choose(workspace.cwd)"
                >
                  <template #leading>
                    <UIcon name="i-lucide-folder" class="size-4 shrink-0 text-muted" aria-hidden="true" />
                  </template>
                  <span class="workspace-option-text">
                    <span>{{ workspace.name }}</span>
                    <span class="workspace-path">{{ workspace.cwd }}</span>
                  </span>
                  <template #trailing>
                    <UIcon v-if="workspace.cwd === cwd" name="i-lucide-check" class="size-4 shrink-0 text-primary" aria-hidden="true" />
                  </template>
                </UButton>
              </div>
            </div>
          </template>
        </UPopover>
      </template>
    </ComposerPanel>
  </section>
</template>

<style scoped>
.workspace-trigger-label { max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.workspace-menu-content { display: flex; width: min(360px, calc(100vw - 24px)); flex-direction: column; padding: var(--space-1); }
.workspace-path-form { display: flex; gap: var(--space-1); padding: var(--space-1); }
.workspace-path-input { min-width: 0; flex: 1; }
.workspace-separator { margin: var(--space-1) 0; border: 0; border-top: 1px solid var(--border); }
.workspace-list { display: flex; min-height: 0; max-height: 240px; flex-direction: column; gap: 2px; overflow-x: hidden; overflow-y: auto; }
.workspace-option { justify-content: flex-start; text-align: left; }
.workspace-option-text { display: flex; min-width: 0; flex: 1; flex-direction: column; align-items: flex-start; }
.workspace-path { max-width: 100%; overflow: hidden; color: var(--muted-foreground); font-family: var(--font-mono); font-size: var(--text-xs); text-overflow: ellipsis; white-space: nowrap; }
</style>
