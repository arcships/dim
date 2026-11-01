<script setup lang="ts">
import { onMounted, ref } from "vue"
import { RouterLink } from "vue-router"
import { useSettingsText } from "../../i18n/settings-general"
import { readSavedSessions, type SavedSessions } from "../../rpc/general-settings"
import { readAck } from "../../rpc/read"
import type { SessionSummary } from "../../rpc/protocol"
import { useConnectionStore } from "../../stores/connection"
import { useSessionsStore } from "../../stores/sessions"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { messageFrom, reloadSettingsOnReconnect } from "./context"

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
async function load(more = false): Promise<void> {
  if (busy.value) return
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
    <form class="settings-card settings-form" role="search" @submit.prevent="load()"><label><span>{{ s('Search history', '搜索历史') }}</span><input v-model="query" class="ui-field" data-size="md" type="search" /></label><label><span>{{ s('Project directory (optional)', '项目目录（可选）') }}</span><input v-model="cwd" class="ui-field" data-size="md" placeholder="/path/to/project" /></label><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy">{{ s('Search', '搜索') }}</button></form>
    <GeneralSettingsStatus :busy="busy" :error="error" @reload="load()" />
    <section v-for="session in page?.sessions" :key="session.sessionId" class="settings-card"><h3>{{ session.preview || session.sessionId }}</h3><p><code>{{ session.cwd }}</code></p><p><time :datetime="new Date(session.updatedAt).toISOString()">{{ new Date(session.updatedAt).toLocaleString() }}</time> · {{ session.model.modelId }}<template v-if="session.running"> · {{ s('Running', '正在运行') }}</template></p><div class="settings-actions"><RouterLink class="ui-button" data-size="md" data-variant="secondary" :to="{ name: 'chat-session', params: { sessionId: session.sessionId } }">{{ s('Reopen', '重新打开') }}</RouterLink><button class="ui-button" data-size="md" data-variant="danger-ghost" :disabled="busy || session.running || !connection.supports('session.delete')" @click="remove(session)">{{ s('Delete permanently', '永久删除') }}</button></div></section>
    <p v-if="page && !page.sessions.length" role="status">{{ s('No matching saved sessions.', '没有匹配的已保存会话。') }}</p><button v-if="page?.nextCursor" class="ui-button" data-size="md" data-variant="secondary" :disabled="busy" @click="load(true)">{{ s('Load more', '加载更多') }}</button>
  </article>
</template>

<style scoped>
h3 { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
code { overflow-wrap: anywhere; }
</style>
