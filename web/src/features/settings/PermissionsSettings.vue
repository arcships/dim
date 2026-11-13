<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readExecutionSettings, readRevision, readRules, readSandboxBackend, type CommandRule, type ExecutionSettings, type RulesView, type SandboxBackend } from "../../rpc/general-settings"
import { RpcError } from "../../rpc/client"
import { useConnectionStore } from "../../stores/connection"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const s = useSettingsText()
const connection = useConnectionStore()
const snapshot = ref<RulesView>()
const rules = ref<CommandRule[]>([])
const busy = ref(false)
const error = ref("")
const saved = ref(false)
const execution = ref<ExecutionSettings>()
const sandbox = ref(false)
const network = ref(false)
const sandboxBackend = ref<SandboxBackend>()
const executionError = ref("")
const executionConflict = ref(false)
const isolationItems = computed(() => [
  { label: s('Off — host files and network', '关闭 — 使用主机文件和网络'), value: "off" },
  { label: s('Sandboxed · No network', '沙盒隔离 · 禁止网络'), value: "isolated" },
  { label: s('Sandboxed · Network allowed', '沙盒隔离 · 允许网络'), value: "network" },
])
const matchItems = computed(() => [
  { label: s('Exact', '精确'), value: "exact" },
  { label: s('Prefix', '前缀'), value: "prefix" },
])
const decisionItems = computed(() => [
  { label: s('Allow', '允许'), value: "allow" },
  { label: s('Deny', '拒绝'), value: "deny" },
])
const isolation = computed({
  get: () => !sandbox.value ? "off" : network.value ? "network" : "isolated",
  set(value: string) {
    sandbox.value = value !== "off"
    if (value !== "off") network.value = value === "network"
  },
})
const executionDirty = computed(() => execution.value !== undefined && (sandbox.value !== execution.value.sandbox || network.value !== execution.value.network))
const rulesDirty = computed(() => snapshot.value !== undefined && JSON.stringify(rules.value) !== JSON.stringify(snapshot.value.commandRules))
const dirty = computed(() => rulesDirty.value || executionDirty.value)
bindSettingsDirty(dirty)

async function load(): Promise<void> {
  if (busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved permission and isolation settings?', '放弃未保存的权限和隔离设置？'))) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    if (!connection.supports("permission.rules")) throw new Error(s('Command rules are not available on this server.', '此服务器不支持命令规则。'))
    snapshot.value = await connection.client.call("permission.rules", {}, readRules)
    rules.value = snapshot.value.commandRules.map((rule) => ({ ...rule }))
  } catch (cause) { error.value = messageFrom(cause) }
  try {
    executionError.value = ""
    if (!connection.supports("settings.read")) throw new Error(s('Process isolation settings are not available on this server.', '此服务器不支持进程隔离设置。'))
    execution.value = await connection.client.call("settings.read", { source: "execution", scope: "user" }, readExecutionSettings)
    sandbox.value = execution.value.sandbox
    network.value = execution.value.network
    executionConflict.value = false
    if (!connection.supports("host.execution.backend")) throw new Error(s('Sandbox backend status is not available on this server.', '此服务器不支持查询沙盒后端状态。'))
    sandboxBackend.value = await connection.client.call("host.execution.backend", {}, readSandboxBackend)
  } catch (cause) { executionError.value = messageFrom(cause) }
  busy.value = false
}

async function save(): Promise<void> {
  if (!snapshot.value || !rulesDirty.value || busy.value) return
  if (rules.value.some((rule) => !rule.command.trim() || /[;&|`()<>$\n\r]/.test(rule.command))) {
    error.value = s('Enter a simple command without shell separators, substitutions or redirection.', '请输入不含 Shell 分隔符、替换或重定向的简单命令。')
    return
  }
  if (rules.value.some((rule) => rule.decision === "allow") && !window.confirm(s('Save command rules that allow execution without asking? Deny presets still take precedence.', '保存允许免询问执行的命令规则？拒绝预设仍然优先。'))) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    const revision = await connection.client.call("permission.set_rules", { expectedRevision: snapshot.value.revision, commandRules: rules.value }, readRevision)
    snapshot.value = { revision, commandRules: rules.value.map((rule) => ({ ...rule })) }
    saved.value = true
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function saveExecution(reset = false): Promise<void> {
  if (!execution.value || busy.value || executionConflict.value || (!reset && !executionDirty.value)) return
  if (reset) {
    if (!window.confirm(s('Reset process isolation to Off? New commands will run without sandbox isolation. Running processes keep their boundary.', '将进程隔离恢复为关闭？新命令将不使用沙盒隔离。已运行的进程保留原边界。'))) return
    sandbox.value = false
    network.value = false
  }
  const widens = execution.value.sandbox && (!sandbox.value || (!execution.value.network && network.value))
  if (!reset && widens && !window.confirm(s('New commands will run with less isolation than now. Running processes keep their boundary.', '新命令将以比现在更少的隔离运行。已运行的进程保留原边界。'))) return
  busy.value = true
  executionError.value = ""
  try {
    execution.value = await connection.client.call("settings.update", {
      source: "execution", scope: "user", expectedRevision: execution.value.revision,
      document: { sandbox: sandbox.value, network: network.value },
    }, readExecutionSettings)
    sandbox.value = execution.value.sandbox
    network.value = execution.value.network
  } catch (cause) {
    executionConflict.value = cause instanceof RpcError && cause.appError === "settings_conflict"
    executionError.value = executionConflict.value ? s('Process isolation settings changed. Reload before saving again; your draft has been preserved.', '进程隔离设置已变更。请重新加载后再保存，当前草稿已保留。') : messageFrom(cause)
  }
  finally { busy.value = false }
}
reloadSettingsOnReconnect(connection, load, dirty)
onMounted(load)
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Permissions', '权限') }}</h2><p>{{ s('User-level command rules apply to process-starting agent tools in every session. Deny wins; Allow never overrides a denied permission preset.', '用户命令规则适用于所有会话中启动进程的代理工具。拒绝优先；允许规则不会越过权限预设的拒绝。') }}</p></header>
    <GeneralSettingsStatus :busy="busy" :error="error" :saved="saved && !dirty" @reload="load" />
    <UAlert v-if="executionError" color="error" variant="subtle" icon="i-lucide-alert-circle" :description="executionError" role="alert" class="settings-banner">
      <template #actions>
        <UButton size="xs" color="neutral" variant="ghost" :disabled="busy" @click="load">{{ s('Reload', '重新加载') }}</UButton>
      </template>
    </UAlert>
    <form v-if="execution" class="settings-card settings-form" @submit.prevent="saveExecution()">
      <h3>{{ s('Process isolation', '进程隔离') }}</h3>
      <p>{{ s('User setting for new commands on this execution host, independent of the session permission preset. Running processes and persistent shells keep their boundary. Without a sandbox backend, sandboxed commands fail rather than run unisolated.', '此执行主机上新命令的用户设置，独立于会话权限预设。已运行的进程和持久 Shell 保留原边界。没有沙盒后端时，需要沙盒的命令会失败，不会降级为无隔离运行。') }}</p>
      <UAlert v-if="sandboxBackend && sandboxBackend.available === false" color="warning" variant="subtle" icon="i-lucide-alert-triangle" :description="`${s('Sandbox is unavailable on this host: ', '此主机上沙盒不可用：')}${sandboxBackend.reason}`" class="settings-banner" />
      <fieldset class="settings-form" :disabled="busy || !connection.supports('settings.update')">
        <UFormField :label="s('Mode', '模式')">
          <USelect v-model="isolation" :items="isolationItems" class="w-full" />
        </UFormField>
      </fieldset>
      <div class="settings-actions">
        <UButton color="neutral" variant="outline" :disabled="busy || executionConflict || !connection.supports('settings.update')" @click="saveExecution(true)">{{ s('Reset to default', '恢复默认') }}</UButton>
        <UButton color="primary" type="submit" :disabled="busy || executionConflict || !executionDirty || !connection.supports('settings.update')">{{ s('Save isolation settings', '保存隔离设置') }}</UButton>
      </div>
    </form>
    <form v-if="snapshot" class="settings-form" @submit.prevent="save">
      <section class="settings-card">
        <h3>{{ s('Command rules', '命令规则') }}</h3>
        <fieldset :disabled="busy || !connection.supports('permission.set_rules')">
          <legend>{{ s('Match commands by words, not wildcards or regular expressions.', '按单词匹配命令，不支持通配符或正则表达式。') }}</legend>
          <div v-for="(rule, index) in rules" :key="index" class="rule-row">
            <UInput v-model="rule.command" :aria-label="s('Command', '命令')" placeholder="git status" required />
            <USelect v-model="rule.match" :items="matchItems" :aria-label="s('Match', '匹配方式')" />
            <USelect v-model="rule.decision" :items="decisionItems" :aria-label="s('Decision', '决策')" />
            <UButton color="error" variant="ghost" @click="rules.splice(index, 1)">{{ s('Remove', '移除') }}</UButton>
          </div>
          <p v-if="!rules.length">{{ s('No command rules. The session permission preset and approval flow decide each call.', '尚无命令规则。每次调用由会话权限预设和审批流程决定。') }}</p>
          <UButton color="neutral" variant="outline" @click="rules.push({ command: '', match: 'prefix', decision: 'deny' })">{{ s('Add rule', '添加规则') }}</UButton>
        </fieldset>
      </section>
      <footer class="settings-actions">
        <UButton color="neutral" variant="outline" :disabled="busy" @click="load">{{ s('Reload', '重新加载') }}</UButton>
        <UButton color="primary" type="submit" :disabled="busy || !rulesDirty || !connection.supports('permission.set_rules')">{{ s('Save rules', '保存规则') }}</UButton>
      </footer>
    </form>
  </article>
</template>

<style scoped>
fieldset { min-width: 0; padding: 0; border: 0; }
label { display: grid; gap: var(--space-2); }
legend { margin-bottom: var(--space-4); color: var(--muted-foreground); font-size: var(--text-sm); }
.rule-row { display: grid; grid-template-columns: minmax(120px, 1fr) auto auto auto; gap: var(--space-2); margin-bottom: var(--space-3); }
@media (max-width: 640px) { .rule-row { grid-template-columns: 1fr 1fr; } .rule-row input { grid-column: 1 / -1; } }
</style>
