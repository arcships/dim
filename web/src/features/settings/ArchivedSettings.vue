<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useRouter } from "vue-router"
import { useSettingsText } from "../../i18n/settings-general"
import { RpcError } from "../../rpc/client"
import type { SessionSummary } from "../../rpc/protocol"
import { readAck, readSummaries } from "../../rpc/read"
import { useArchiveStore } from "../../stores/archive"
import { useConnectionStore } from "../../stores/connection"
import { useSessionsStore, workspaceName } from "../../stores/sessions"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { messageFrom, reloadSettingsOnReconnect } from "./context"

const ALL_WORKSPACES = "__all__"
const s = useSettingsText()
const router = useRouter()
const archive = useArchiveStore()
const connection = useConnectionStore()
const sessions = useSessionsStore()
/** Every row the daemon stores, closed ones included; the browser decides which of them are hidden. */
const rows = ref<SessionSummary[]>([])
/** True only for rows from a completed successful read; a pending or failed read proves nothing. */
const listingValid = ref(false)
const busy = ref(false)
const error = ref("")
const actionError = ref("")
const report = ref("")
const rowErrors = ref<Record<string, string>>({})
const query = ref("")
const cwd = ref("")

const archived = computed(() => rows.value.filter((row) => archive.has(row.sessionId)))
const unavailable = computed(() => listingValid.value
  ? archive.hidden.filter((id) => !rows.value.some((row) => row.sessionId === id)).length
  : 0)
const visible = computed(() => {
  const needle = query.value.trim().toLocaleLowerCase()
  return archived.value.filter((row) => {
    if (cwd.value !== "" && row.cwd !== cwd.value) return false
    return needle === "" || [row.title, row.preview, workspaceName(row.cwd), row.cwd].some((text) => text?.toLocaleLowerCase().includes(needle))
  })
})
const workspaceItems = computed(() => {
  const byCwd = new Map<string, string>()
  for (const row of archived.value) byCwd.set(row.cwd, workspaceName(row.cwd))
  return [
    { label: s("All workspaces", "所有工作区"), value: ALL_WORKSPACES },
    ...[...byCwd].map(([path, name]) => ({ label: `${name} — ${path}`, value: path })),
  ]
})
const canDelete = computed(() => connection.phase === "connected" && connection.supports("session.delete"))
const deleteBlocked = s("Deleting needs a connection that serves session.delete.", "删除需要支持 session.delete 的连接。")

function label(row: SessionSummary): string {
  return row.title ?? row.preview ?? row.sessionId
}

/** The current directory, then the read an App Server without `includeClosed` accepts. */
async function readArchived(): Promise<SessionSummary[]> {
  try {
    return await connection.client.call("session.summaries", { includeClosed: true }, readSummaries)
  } catch (cause) {
    if (!(cause instanceof RpcError) || cause.appError !== "invalid_params") throw cause
    return await connection.client.call("session.summaries", {}, readSummaries)
  }
}

function pruneFilter(): void {
  if (cwd.value !== "" && !archived.value.some((row) => row.cwd === cwd.value)) cwd.value = ""
}

async function load(): Promise<void> {
  if (busy.value) return
  busy.value = true
  error.value = ""
  listingValid.value = false
  try {
    rows.value = await readArchived()
    listingValid.value = true
    pruneFilter()
  } catch (cause) {
    error.value = messageFrom(cause)
  } finally {
    busy.value = false
  }
}

function storageFailed(): void {
  actionError.value = s("This browser cannot store the archive list.", "此浏览器无法保存归档列表。")
}

function restore(row: SessionSummary): boolean {
  if (!archive.restore(row.sessionId)) {
    storageFailed()
    return false
  }
  actionError.value = ""
  report.value = ""
  pruneFilter()
  return true
}

function open(row: SessionSummary): void {
  if (restore(row)) void router.push({ name: "chat-session", params: { sessionId: row.sessionId } })
}

function confirmDelete(targets: SessionSummary[]): boolean {
  const names = targets.slice(0, 3).map((row) => `“${label(row)}”`).join(", ")
  return window.confirm(targets.length === 1
    ? s(`Permanently delete “${label(targets[0]!)}” and all its history? This cannot be undone.`, `永久删除“${label(targets[0]!)}”及其全部历史？此操作不可撤销。`)
    : s(`Permanently delete all ${targets.length} archived sessions (${names}${targets.length > 3 ? ", …" : ""}) and their history? This cannot be undone.`, `永久删除全部 ${targets.length} 个已归档会话（${names}${targets.length > 3 ? " 等" : ""}）及其历史？此操作不可撤销。`))
}

/** One call per target; a failure leaves that row on the page and the loop continues. */
async function deleteRows(targets: SessionSummary[], bulk: boolean): Promise<void> {
  if (busy.value || targets.length === 0 || !canDelete.value) return
  busy.value = true
  actionError.value = ""
  report.value = ""
  rowErrors.value = {}
  let deleted = 0
  for (const target of targets) {
    try {
      await connection.client.call("session.delete", { sessionId: target.sessionId, requireInactive: true }, readAck)
      deleted += 1
      if (!archive.restore(target.sessionId)) storageFailed()
      sessions.forget(target.sessionId)
      rows.value = rows.value.filter((row) => row.sessionId !== target.sessionId)
    } catch (cause) {
      rowErrors.value[target.sessionId] = cause instanceof RpcError && cause.appError === "session_active"
        ? s("This Session is in use and was not deleted.", "此会话正在使用中，未删除。")
        : messageFrom(cause)
    }
  }
  report.value = bulk
    ? s(`Deleted ${deleted} of ${targets.length} archived sessions.`, `已删除 ${deleted}/${targets.length} 个已归档会话。`)
    : deleted === 1 ? s("Deleted the archived session.", "已删除该归档会话。") : ""
  pruneFilter()
  busy.value = false
}

function deleteAll(): void {
  if (confirmDelete(archived.value)) void deleteRows(archived.value, true)
}

function dropUnavailable(): void {
  if (busy.value || !listingValid.value) return
  const available = new Set(rows.value.map((row) => row.sessionId))
  if (!archive.drop(archive.hidden.filter((id) => !available.has(id)))) storageFailed()
}

reloadSettingsOnReconnect(connection, () => load())
onMounted(() => load())
</script>

<template>
  <article class="settings-page">
    <header>
      <h2 data-route-heading tabindex="-1">{{ s('Archived sessions', '已归档会话') }}</h2>
      <p>{{ s('Sessions this browser hides from the sidebar. Their history stays with the daemon, and Restore or Open shows a Session again.', '此浏览器从侧栏收起的会话。历史仍保存在 daemon 上，点击恢复或打开即可重新显示。') }}</p>
    </header>
    <div class="settings-card settings-form">
      <UFormField :label="s('Search archived sessions', '搜索已归档会话')">
        <UInput v-model="query" type="search" class="w-full" />
      </UFormField>
      <UFormField :label="s('Workspace filter', '工作区筛选')">
        <USelect :model-value="cwd || ALL_WORKSPACES" :items="workspaceItems" class="w-full" @update:model-value="cwd = $event === ALL_WORKSPACES ? '' : String($event)" />
      </UFormField>
    </div>
    <GeneralSettingsStatus :busy="busy" :error="error" @reload="load()" />
    <UAlert v-if="actionError" class="settings-banner" color="error" variant="subtle" :description="actionError" role="alert" />
    <UAlert v-if="report" class="settings-banner" color="neutral" variant="subtle" :description="report" role="status" />
    <section v-for="row in visible" :key="row.sessionId" class="settings-card">
      <h3>{{ label(row) }}</h3>
      <p><code>{{ row.cwd }}</code></p>
      <p>
        <time :datetime="new Date(row.updatedAt).toISOString()">{{ new Date(row.updatedAt).toLocaleString() }}</time>
        · {{ row.model.modelId }}
        <template v-if="row.running"> · {{ s('Running', '正在运行') }}</template>
        <template v-else-if="row.closed"> · {{ s('Closed in this daemon', '已在本进程关闭') }}</template>
      </p>
      <p v-if="rowErrors[row.sessionId]" class="settings-row-error" role="alert">{{ rowErrors[row.sessionId] }}</p>
      <div class="settings-actions">
        <UButton color="neutral" variant="outline" @click="restore(row)">{{ s('Restore', '恢复') }}</UButton>
        <UButton v-if="!row.closed" color="neutral" variant="outline" @click="open(row)">{{ s('Open', '打开') }}</UButton>
        <UButton color="error" variant="ghost" :disabled="busy || !canDelete || row.running" :title="canDelete ? undefined : deleteBlocked" @click="confirmDelete([row]) && deleteRows([row], false)">{{ s('Delete permanently', '永久删除') }}</UButton>
      </div>
    </section>
    <p v-if="listingValid && !busy && archived.length === 0" role="status">{{ s('No archived sessions in this browser.', '此浏览器还没有归档会话。') }}</p>
    <p v-else-if="listingValid && archived.length > 0 && visible.length === 0" role="status">{{ s('No matching archived sessions.', '没有匹配的已归档会话。') }}</p>
    <section v-if="listingValid && unavailable > 0" class="settings-card">
      <p>{{ s(`${unavailable} archived entries are not stored by this daemon.`, `${unavailable} 个归档条目不在当前 daemon 中。`) }}</p>
      <div class="settings-actions">
        <UButton color="neutral" variant="outline" :disabled="busy" @click="dropUnavailable">{{ s('Remove unavailable', '移除不可用条目') }}</UButton>
      </div>
    </section>
    <div v-if="archived.length > 0" class="settings-actions">
      <UButton color="error" variant="ghost" :disabled="busy || !canDelete" :title="canDelete ? undefined : deleteBlocked" @click="deleteAll">{{ s('Delete all archived', '删除全部已归档会话') }}</UButton>
    </div>
  </article>
</template>

<style scoped>
h3 { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
code { overflow-wrap: anywhere; }
.settings-row-error { color: var(--ui-error); }
</style>
