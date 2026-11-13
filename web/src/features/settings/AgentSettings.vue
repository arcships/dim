<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readDefaultSelection, readExecShell, readModelPolicy, readRevision, readSelectedModel, type DefaultSelection, type ExecShellView, type ModelPolicyView } from "../../rpc/general-settings"
import { useConnectionStore } from "../../stores/connection"
import { useModelsStore } from "../../stores/models"
import { useSessionsStore } from "../../stores/sessions"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const USER_SCOPE = "__user__"
const AUTO_SHELL = "__auto__"
const s = useSettingsText()
const connection = useConnectionStore()
const models = useModelsStore()
const sessions = useSessionsStore()
const snapshot = ref<DefaultSelection>()
const policy = ref<ModelPolicyView>()
const memberModel = ref("agent")
const shell = ref<ExecShellView>()
const shellPath = ref("")
const loaded = ref(false)
const loadedCwd = ref("")
const selected = ref("")
const busy = ref(false)
const error = ref("")
const saved = ref(false)
const options = computed(() => models.providers?.flatMap((provider) => provider.models.map((model) => ({ key: JSON.stringify([provider.providerId, model.id]), providerId: provider.providerId, modelId: model.id, label: `${provider.name} / ${model.name}` }))) ?? [])
const current = computed(() => snapshot.value ? JSON.stringify([snapshot.value.providerId, snapshot.value.modelId]) : "")
const currentPolicy = computed(() => policy.value?.policy.mode === "fixed" ? JSON.stringify([policy.value.policy.model.providerId, policy.value.policy.model.modelId]) : policy.value?.policy.mode ?? "agent")
const scopeItems = computed(() => [
  { label: s('User default · all workspaces', '用户默认 · 所有工作区'), value: USER_SCOPE },
  ...sessions.workspaces.map((workspace) => ({ label: `${workspace.name} — ${workspace.cwd}`, value: workspace.cwd })),
])
const modelItems = computed(() => [
  ...(current.value && !options.value.some((option) => option.key === current.value)
    ? [{ label: `${snapshot.value?.providerId} / ${snapshot.value?.modelId} (${s('unavailable', '不可用')})`, value: current.value, disabled: true }]
    : []),
  ...options.value.map((option) => ({ label: option.label, value: option.key })),
])
const policyItems = computed(() => [
  { label: s('Agent decides (profile / workspace defaults)', '由代理决定（配置 / 工作区默认值）'), value: "agent" },
  { label: s('Inherit requesting branch model', '继承请求分支的模型'), value: "inherit" },
  ...(currentPolicy.value !== "agent" && currentPolicy.value !== "inherit" && !options.value.some((option) => option.key === currentPolicy.value)
    ? [{ label: s('Configured model unavailable', '已配置模型不可用'), value: currentPolicy.value, disabled: true }]
    : []),
  ...options.value.map((option) => ({ label: option.label, value: option.key })),
])
const shellItems = computed(() => [
  { label: s('Automatic detection', '自动检测'), value: AUTO_SHELL },
  ...(shellPath.value && !shell.value?.shells.some((choice) => choice.executablePath === shellPath.value)
    ? [{ label: `${shellPath.value} (${s('unavailable', '不可用')})`, value: shellPath.value, disabled: true }]
    : []),
  ...(shell.value?.shells.map((choice) => ({ label: `${choice.name} — ${choice.executablePath}`, value: choice.executablePath })) ?? []),
])
const dirty = computed(() => loaded.value && (selected.value !== current.value || memberModel.value !== currentPolicy.value || (shell.value !== undefined && shellPath.value !== (shell.value.executablePath ?? ""))))
bindSettingsDirty(dirty)
async function load(path = loadedCwd.value): Promise<void> {
  if (busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved defaults?', '放弃未保存的默认设置？'))) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    if (!connection.supports("provider.selection")) throw new Error(s('Default model selection is not available on this server.', '此服务器不支持默认模型选择。'))
    const [next, nextPolicy, nextShell] = await Promise.all([connection.client.call("provider.selection", path ? { cwd: path } : {}, readDefaultSelection), connection.supports("collaboration.model_policy") ? connection.client.call("collaboration.model_policy", {}, readModelPolicy) : undefined, connection.supports("host.exec_shell.get") ? connection.client.call("host.exec_shell.get", {}, readExecShell) : undefined, models.load()])
    snapshot.value = next
    policy.value = nextPolicy
    shell.value = nextShell
    shellPath.value = nextShell?.executablePath ?? ""
    memberModel.value = currentPolicy.value
    loadedCwd.value = path
    selected.value = current.value
    loaded.value = true
    if (models.error) error.value = models.error
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function save(): Promise<void> {
  const target = options.value.find((option) => option.key === selected.value)
  if (busy.value || !dirty.value) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    if (selected.value !== current.value && target) {
      snapshot.value = await connection.client.call("provider.select", { ...(loadedCwd.value ? { cwd: loadedCwd.value } : {}), providerId: target.providerId, modelId: target.modelId, ...(snapshot.value ? { expectedVersion: snapshot.value.version } : {}) }, readSelectedModel)
      selected.value = current.value
    }
    if (policy.value && memberModel.value !== currentPolicy.value) {
      const fixed = options.value.find((option) => option.key === memberModel.value)
      const next = memberModel.value === "agent" || memberModel.value === "inherit" ? { mode: memberModel.value } as const : fixed ? { mode: "fixed", model: { providerId: fixed.providerId, modelId: fixed.modelId } } as const : undefined
      if (!next) throw new Error(s('Choose an available subagent model.', '请选择可用的子代理模型。'))
      const revision = await connection.client.call("collaboration.set_model_policy", { expectedRevision: policy.value.revision, policy: next }, readRevision)
      policy.value = { revision, policy: next }
    }
    if (shell.value && shellPath.value !== (shell.value.executablePath ?? "")) {
      shell.value = await connection.client.call("host.exec_shell.update", { expectedRevision: shell.value.revision, ...(shellPath.value ? { executablePath: shellPath.value } : {}) }, readExecShell)
      shellPath.value = shell.value.executablePath ?? ""
    }
    saved.value = true
  } catch (cause) { error.value = messageFrom(cause) }
  finally { busy.value = false }
}
async function changeScope(value: unknown): Promise<void> {
  await load(value === USER_SCOPE ? "" : String(value ?? ""))
}
reloadSettingsOnReconnect(connection, () => load(), dirty)
onMounted(() => load())
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Agent', '代理') }}</h2><p>{{ s('Choose the default model for new sessions. Existing sessions keep their selected model.', '为新会话选择默认模型。现有会话保留其已选模型。') }}</p></header>
    <div class="settings-card settings-form">
      <UFormField :label="s('Default model scope', '默认模型范围')">
        <USelect :model-value="loadedCwd || USER_SCOPE" :items="scopeItems" :disabled="busy" class="w-full" @update:model-value="changeScope" />
      </UFormField>
      <p>{{ s('A workspace default overrides the user default for new sessions in that workspace. Subagent policy and command shell below always apply to all workspaces.', '工作区默认值覆盖该工作区新会话使用的用户默认值。下方的子代理策略和命令 Shell 始终适用于所有工作区。') }}</p>
      <UButton color="neutral" variant="outline" :disabled="busy" class="justify-self-start" @click="load()">{{ s('Reload defaults', '重新加载默认设置') }}</UButton>
    </div>
    <GeneralSettingsStatus :busy="busy" :error="error" :saved="saved && !dirty" @reload="load()" />
    <form v-if="loaded" class="settings-form" @submit.prevent="save">
      <section class="settings-card">
        <h3>{{ s('Default model', '默认模型') }}</h3>
        <p>{{ loadedCwd || s('Global default', '全局默认') }}</p>
        <UFormField :label="s('Model', '模型')">
          <USelect :model-value="selected || undefined" :items="modelItems" :placeholder="s('Automatic fallback — choose a model to pin a default', '自动回退 — 选择模型以固定默认值')" :disabled="busy || !connection.supports('provider.select')" class="w-full" @update:model-value="selected = String($event)" />
        </UFormField>
        <p v-if="!options.length">{{ s('No selectable models. Configure a provider and its model limits first.', '没有可选模型。请先配置提供商和模型限制。') }}</p>
      </section>
      <section v-if="policy" class="settings-card">
        <h3>{{ s('Subagent models', '子代理模型') }}</h3>
        <p>{{ s('Applies to every newly created native Collaboration member, including named agents. Existing members and external agents are unchanged.', '适用于每个新建的原生协作成员，包括命名代理。现有成员和外部代理不受影响。') }}</p>
        <UFormField :label="s('Model policy', '模型策略')">
          <USelect v-model="memberModel" :items="policyItems" :disabled="busy || !connection.supports('collaboration.set_model_policy')" class="w-full" />
        </UFormField>
      </section>
      <section v-if="shell" class="settings-card">
        <h3>{{ s('Command shell', '命令 Shell') }}</h3>
        <p>{{ s('Used for new command invocations. Already running processes keep their shell.', '用于新启动的命令。已运行的进程保留其原 Shell。') }}</p>
        <UFormField :label="s('Executable', '可执行程序')">
          <USelect :model-value="shellPath || AUTO_SHELL" :items="shellItems" :disabled="busy || !connection.supports('host.exec_shell.update')" class="w-full" @update:model-value="shellPath = $event === AUTO_SHELL ? '' : String($event)" />
        </UFormField>
        <UAlert v-if="shell.configError" color="error" variant="subtle" icon="i-lucide-alert-circle" :description="shell.configError" class="settings-banner mt-4" />
      </section>
      <footer class="settings-actions">
        <UButton color="primary" type="submit" :disabled="busy || !dirty">{{ s('Save defaults', '保存默认设置') }}</UButton>
      </footer>
    </form>
  </article>
</template>
