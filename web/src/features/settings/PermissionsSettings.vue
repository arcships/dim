<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readExecutionSettings, readRevision, readRules, readSandboxBackend, type CommandRule, type ExecutionSettings, type RulesView, type SandboxBackend } from "../../rpc/general-settings"
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
const executionDirty = computed(() => execution.value !== undefined && (sandbox.value !== execution.value.sandbox || network.value !== execution.value.network))
const rulesDirty = computed(() => snapshot.value !== undefined && JSON.stringify(rules.value) !== JSON.stringify(snapshot.value.commandRules))
const dirty = computed(() => rulesDirty.value || executionDirty.value)
bindSettingsDirty(dirty)

async function load(): Promise<void> {
  if (busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved command rules?', '放弃未保存的命令规则？'))) return
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
    if (connection.supports("settings.read")) {
      execution.value = await connection.client.call("settings.read", { source: "execution", scope: "user" }, readExecutionSettings)
      sandbox.value = execution.value.sandbox
      network.value = execution.value.network
    }
    if (connection.supports("host.execution.backend")) {
      sandboxBackend.value = await connection.client.call("host.execution.backend", {}, readSandboxBackend)
    }
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
async function saveExecution(): Promise<void> {
  if (!execution.value || busy.value || !executionDirty.value) return
  const widens = (execution.value.sandbox && !sandbox.value) || (!execution.value.network && network.value)
  if (widens && !window.confirm(s('New commands will run with less isolation than now. Running processes keep their boundary.', '新命令将以比现在更少的隔离运行。已运行的进程保留原边界。'))) return
  busy.value = true
  executionError.value = ""
  try {
    execution.value = await connection.client.call("settings.update", {
      source: "execution", scope: "user", expectedRevision: execution.value.revision,
      document: { sandbox: sandbox.value, network: network.value },
    }, readExecutionSettings)
    sandbox.value = execution.value.sandbox
    network.value = execution.value.network
  } catch (cause) { executionError.value = messageFrom(cause) }
  finally { busy.value = false }
}
reloadSettingsOnReconnect(connection, load, dirty)
onMounted(load)
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Permissions', '权限') }}</h2><p>{{ s('User-level command rules apply to process-starting agent tools in every session. Deny wins; Allow never overrides a denied permission preset.', '用户命令规则适用于所有会话中启动进程的代理工具。拒绝优先；允许规则不会越过权限预设的拒绝。') }}</p></header>
    <GeneralSettingsStatus :busy="busy" :error="error" :saved="saved && !dirty" @reload="load" />
    <p v-if="executionError" class="ui-banner settings-banner" data-tone="danger" role="alert">{{ executionError }} <button class="ui-button" data-variant="ghost" :disabled="busy" @click="load">{{ s('Reload', '重新加载') }}</button></p>
    <form v-if="execution" class="settings-card settings-form" @submit.prevent="saveExecution()">
      <h3>{{ s('Execution', '执行') }}</h3>
      <p>{{ s('Applies to new commands on this execution host. Running processes keep their boundary. A platform without a sandbox backend fails the command instead of running it unsandboxed.', '适用于此执行主机上的新命令。已运行的进程保留原边界。没有沙盒后端的平台会让命令失败，而不是不加隔离地运行。') }}</p>
      <p v-if="sandboxBackend && sandboxBackend.available === false" class="ui-banner settings-banner" data-tone="warning">{{ s('Sandbox is unavailable on this host: ', '此主机上沙盒不可用：') }}{{ sandboxBackend.reason }}</p>
      <fieldset class="settings-form" :disabled="busy || !connection.supports('settings.update')">
        <label><input v-model="sandbox" type="checkbox" />{{ s('Sandbox — run commands isolated from the host', '沙盒 — 在隔离环境中运行命令') }}</label>
        <label><input v-model="network" type="checkbox" />{{ s('Network — allow outbound connections while sandboxed', '网络 — 沙盒内允许出站连接') }}</label>
      </fieldset>
      <p class="ui-banner settings-banner" data-tone="warning">{{ s('Network applies only while the sandbox is on. With the sandbox off, commands run as the daemon user with this host\'s files and network.', '网络仅在沙盒开启时生效。沙盒关闭时，命令以守护进程用户的身份、使用此主机的文件和网络运行。') }}</p>
      <div class="settings-actions"><button class="ui-button" data-variant="primary" :disabled="busy || !executionDirty || !connection.supports('settings.update')">{{ s('Save execution settings', '保存执行设置') }}</button></div>
    </form>
    <form v-if="snapshot" class="settings-form" @submit.prevent="save">
      <section class="settings-card">
        <h3>{{ s('Command rules', '命令规则') }}</h3>
        <fieldset :disabled="busy || !connection.supports('permission.set_rules')">
          <legend>{{ s('Match commands by words, not wildcards or regular expressions.', '按单词匹配命令，不支持通配符或正则表达式。') }}</legend>
          <div v-for="(rule, index) in rules" :key="index" class="rule-row">
            <input v-model="rule.command" class="ui-field" data-size="md" :aria-label="s('Command', '命令')" placeholder="git status" required />
            <select v-model="rule.match" class="ui-field" data-size="md" :aria-label="s('Match', '匹配方式')"><option value="exact">{{ s('Exact', '精确') }}</option><option value="prefix">{{ s('Prefix', '前缀') }}</option></select>
            <select v-model="rule.decision" class="ui-field" data-size="md" :aria-label="s('Decision', '决策')"><option value="allow">{{ s('Allow', '允许') }}</option><option value="deny">{{ s('Deny', '拒绝') }}</option></select>
            <button class="ui-button" data-size="md" data-variant="danger-ghost" type="button" @click="rules.splice(index, 1)">{{ s('Remove', '移除') }}</button>
          </div>
          <p v-if="!rules.length">{{ s('No command rules. The session permission preset and approval flow decide each call.', '尚无命令规则。每次调用由会话权限预设和审批流程决定。') }}</p>
          <button class="ui-button" data-size="md" data-variant="secondary" type="button" @click="rules.push({ command: '', match: 'prefix', decision: 'deny' })">{{ s('Add rule', '添加规则') }}</button>
        </fieldset>
      </section>
      <footer class="settings-actions"><button class="ui-button" data-size="md" data-variant="secondary" type="button" :disabled="busy" @click="load">{{ s('Reload', '重新加载') }}</button><button class="ui-button" data-size="md" data-variant="primary" :disabled="busy || !rulesDirty || !connection.supports('permission.set_rules')">{{ s('Save rules', '保存规则') }}</button></footer>
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
