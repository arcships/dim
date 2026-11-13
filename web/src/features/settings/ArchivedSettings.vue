<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readSavedSessions, type SavedSessions } from "../../rpc/general-settings"
import { readAck } from "../../rpc/read"
import type { SessionSummary } from "../../rpc/protocol"
import { useConnectionStore } from "../../stores/connection"
import { useSessionsStore } from "../../stores/sessions"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { messageFrom, reloadSettingsOnReconnect } from "./context"

const ALL_WORKSPACES = "__all__"
const s = useSettingsText()
const connection = useConnectionStore()
const sessions = useSessionsStore()
const page = ref<SavedSessions>()
const query = ref("")
const cwd = ref("")
const loadedQuery = ref("")
const loadedCwd = ref("")
const busy = ref(false)
const error = ref("")
const filtersChanged = computed(() => query.value.trim() !== loadedQuery.value || cwd.value.trim() !== loadedCwd.value)
const workspaceItems = computed(() => [
  { label: s('All workspaces', '所有工作区'), value: ALL_WORKSPACES },
  ...sessions.workspaces.map((workspace) => ({ label: `${workspace.name} — ${workspace.cwd}`, value: workspace.cwd })),
])
async function load(more = false): Promise<void> {
  if (busy.value || (more && filtersChanged.value)) return
  busy.value = true
  error.value = ""
  try {
    const search = more ? loadedQuery.value : query.value.trim()
    const path = more ? loadedCwd.value : cwd.value.trim()
    const result = await connection.client.call("session.catalog", { limit: 50, withPreview: true, ...(search ? { query: search } : {}), ...(path ? { cwd: path } : {}), ...(more && page.value?.nextCursor ? { cursor: page.value.nextCursor } : {}) }, readSavedSessions)
    page.value = { sessions: more ? [...(page.value?.sessions ?? []), ...result.sessions] : result.sessions, nextCursor: result.nextCursor }
    loadedQuery.value = search
    loadedCwd.value = path
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function remove(session: SessionSummary): Promise<void> {
  if (busy.value || !window.confirm(s('Permanently delete this session and all its history? This cannot be undone.', '永久删除此会话及其全部历史？此操作不可撤销。'))) return
  busy.value = true
  error.value = ""
  try {
    await connection.client.call("session.delete", { sessionId: session.sessionId }, readAck)
    if (page.value) page.value.sessions = page.value.sessions.filter((item) => item.sessionId !== session.sessionId)
    sessions.forget(session.sessionId)
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
reloadSettingsOnReconnect(connection, () => load())
onMounted(() => load())
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Saved sessions', '已保存的会话') }}</h2><p>{{ s('Browse persisted history, reopen sessions or permanently delete them. This runtime does not have a separate archived flag; closing a session keeps its saved history.', '浏览持久化历史、重新打开会话或永久删除会话。当前运行时没有单独的归档标记；关闭会话会保留其已保存历史。') }}</p></header>
    <form class="settings-card settings-form" role="search" @submit.prevent="load()">
      <UFormField :label="s('Search history', '搜索历史')">
        <UInput v-model="query" type="search" class="w-full" />
      </UFormField>
      <UFormField :label="s('Workspace filter', '工作区筛选')">
        <USelect :model-value="cwd || ALL_WORKSPACES" :items="workspaceItems" :disabled="busy" class="w-full" @update:model-value="cwd = $event === ALL_WORKSPACES ? '' : String($event)" />
      </UFormField>
      <p>{{ s('Only filters saved sessions; this does not change any project settings.', '仅筛选已保存的会话，不会更改任何项目设置。') }}</p>
      <UButton color="neutral" variant="outline" type="submit" :disabled="busy" class="justify-self-start">{{ s('Search', '搜索') }}</UButton>
    </form>
    <GeneralSettingsStatus :busy="busy" :error="error" @reload="load()" />
    <section v-for="session in page?.sessions" :key="session.sessionId" class="settings-card">
      <h3>{{ session.title ?? (session.preview || session.sessionId) }}</h3>
      <p><code>{{ session.cwd }}</code></p>
      <p><time :datetime="new Date(session.updatedAt).toISOString()">{{ new Date(session.updatedAt).toLocaleString() }}</time> · {{ session.model.modelId }}<template v-if="session.running"> · {{ s('Running', '正在运行') }}</template></p>
      <div class="settings-actions">
        <UButton color="neutral" variant="outline" :to="{ name: 'chat-session', params: { sessionId: session.sessionId } }">{{ s('Reopen', '重新打开') }}</UButton>
        <UButton color="error" variant="ghost" :disabled="busy || session.running || !connection.supports('session.delete')" @click="remove(session)">{{ s('Delete permanently', '永久删除') }}</UButton>
      </div>
    </section>
    <p v-if="page && !page.sessions.length" role="status">{{ s('No matching saved sessions.', '没有匹配的已保存会话。') }}</p>
    <UButton v-if="page?.nextCursor" color="neutral" variant="outline" :disabled="busy || filtersChanged" @click="load(true)">{{ s('Load more', '加载更多') }}</UButton>
  </article>
</template>

<style scoped>
h3 { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
code { overflow-wrap: anywhere; }
</style>
