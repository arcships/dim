<script setup lang="ts">
import { computed, watch } from "vue"
import { RouterLink } from "vue-router"
import { useI18n } from "vue-i18n"
import { Menu } from "lucide-vue-next"
import ApprovalPrompt from "./ApprovalPrompt.vue"
import ComposerPanel from "./ComposerPanel.vue"
import TimelineView from "./TimelineView.vue"
import type { ModelId } from "../../rpc/protocol"
import { useChatStore } from "../../stores/chat"
import { useConnectionStore } from "../../stores/connection"
import { useSessionsStore, workspaceName } from "../../stores/sessions"
import { useUiStore } from "../../stores/ui"

const props = defineProps<{ sessionId: string }>()
const { t } = useI18n()
const chat = useChatStore()
const connection = useConnectionStore()
const sessions = useSessionsStore()
const ui = useUiStore()

watch(() => props.sessionId, (id) => chat.open(id), { immediate: true })

const view = computed(() => chat.views[props.sessionId])
const summary = computed(() => sessions.byId.get(props.sessionId))
const title = computed(() => summary.value?.preview ?? t("sidebar.untitled"))
const approval = computed(() => view.value?.approvals[0])

function send(text: string): Promise<void> {
  return chat.submit(props.sessionId, text)
}

function stop(): Promise<void> {
  return chat.stop(props.sessionId)
}

function selectModel(model: ModelId): Promise<void> {
  return chat.setModel(props.sessionId, model)
}
</script>

<template>
  <section v-if="view?.status === 'closed'" class="removed-state route-page">
    <h1 data-route-heading tabindex="-1">{{ t("chat.removedTitle") }}</h1>
    <p>{{ t("chat.removedDescription") }}</p>
    <RouterLink to="/chat/new">{{ t("sidebar.newSession") }}</RouterLink>
  </section>

  <section v-else-if="view?.status === 'failed'" class="removed-state route-page">
    <h1 data-route-heading tabindex="-1">{{ title }}</h1>
    <p role="alert">{{ view.error }}</p>
  </section>

  <section v-else-if="!view || view.status === 'loading'" class="chat-loading" aria-busy="true">
    <h1 class="sr-only">{{ t("route.session") }}</h1><p>{{ t("common.loading") }}</p>
  </section>

  <section v-else class="chat-page">
    <div class="chat-page-top">
      <header class="session-header">
        <button v-if="ui.shellMode === 'compact'" class="ui-icon-button session-nav" data-variant="ghost" data-size="md" type="button" :aria-label="t('sidebar.openMenu')" @click="ui.drawerOpen = true"><Menu aria-hidden="true" /></button>
        <div class="session-heading">
          <template v-if="summary">
            <span class="workspace-crumb" :title="summary.cwd">{{ workspaceName(summary.cwd) }}</span>
            <span class="workspace-crumb-separator" aria-hidden="true">/</span>
          </template>
          <h1 data-route-heading tabindex="-1" :title="title">{{ title }}</h1>
        </div>
        <span v-if="view.running" class="ui-badge session-running" data-tone="primary"><span class="status-dot" data-state="running" aria-hidden="true" />{{ t(view.stopping ? "status.stopping" : "status.running") }}</span>
      </header>
    </div>
    <TimelineView :rows="view.rows" :running="view.running" />
    <div v-if="approval" class="composer-panel">
      <ApprovalPrompt :key="approval.requestId" :session-id="sessionId" :request="approval" />
    </div>
    <ComposerPanel
      v-else
      :running="view.running"
      :stopping="view.stopping"
      :model="view.model"
      :send="send"
      :stop="stop"
      :model-selectable="connection.supports('session.model.update')"
      :select-model="selectModel"
    />
  </section>
</template>

<style scoped>
.session-running { gap: 6px; }
.session-running > .status-dot { border: 0; }
</style>
