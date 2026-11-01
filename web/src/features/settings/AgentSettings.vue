<script setup lang="ts">
import { computed, onMounted, ref } from "vue"
import { useSettingsText } from "../../i18n/settings-general"
import { readDefaultSelection, readExecShell, readModelPolicy, readRevision, readSelectedModel, type DefaultSelection, type ExecShellView, type ModelPolicyView } from "../../rpc/general-settings"
import { useConnectionStore } from "../../stores/connection"
import { useModelsStore } from "../../stores/models"
import GeneralSettingsStatus from "./GeneralSettingsStatus.vue"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"

const s = useSettingsText()
const connection = useConnectionStore()
const models = useModelsStore()
const snapshot = ref<DefaultSelection>()
const policy = ref<ModelPolicyView>()
const memberModel = ref("agent")
const shell = ref<ExecShellView>()
const shellPath = ref("")
const loaded = ref(false)
const cwd = ref("")
const loadedCwd = ref("")
const selected = ref("")
const busy = ref(false)
const error = ref("")
const saved = ref(false)
const options = computed(() => models.providers?.flatMap((provider) => provider.models.map((model) => ({ key: JSON.stringify([provider.providerId, model.id]), providerId: provider.providerId, modelId: model.id, label: `${provider.name} / ${model.name}` }))) ?? [])
const current = computed(() => snapshot.value ? JSON.stringify([snapshot.value.providerId, snapshot.value.modelId]) : "")
const currentPolicy = computed(() => policy.value?.policy.mode === "fixed" ? JSON.stringify([policy.value.policy.model.providerId, policy.value.policy.model.modelId]) : policy.value?.policy.mode ?? "agent")
const dirty = computed(() => loaded.value && (selected.value !== current.value || memberModel.value !== currentPolicy.value || (shell.value !== undefined && shellPath.value !== (shell.value.executablePath ?? ""))))
bindSettingsDirty(dirty)
async function load(): Promise<void> {
  if (busy.value) return
  if (dirty.value && !window.confirm(s('Discard unsaved defaults?', '放弃未保存的默认设置？'))) return
  busy.value = true
  error.value = ""
  saved.value = false
  try {
    if (!connection.supports("provider.selection")) throw new Error(s('Default model selection is not available on this server.', '此服务器不支持默认模型选择。'))
    const path = cwd.value.trim()
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
reloadSettingsOnReconnect(connection, load, dirty)
onMounted(load)
</script>

<template>
  <article class="settings-page">
    <header><h2 data-route-heading tabindex="-1">{{ s('Agent', '代理') }}</h2><p>{{ s('Choose the default model for new sessions. Existing sessions keep their selected model.', '为新会话选择默认模型。现有会话保留其已选模型。') }}</p></header>
    <form class="settings-card settings-form" @submit.prevent="load"><label><span>{{ s('Project directory (blank for the global default)', '项目目录（留空表示全局默认）') }}</span><input v-model="cwd" class="ui-field" data-size="md" :disabled="busy" placeholder="/path/to/project" /></label><button class="ui-button" data-size="md" data-variant="secondary" :disabled="busy">{{ s('Load defaults', '加载默认设置') }}</button></form>
    <GeneralSettingsStatus :busy="busy" :error="error" :saved="saved && !dirty" @reload="load" />
    <form v-if="loaded" class="settings-form" @submit.prevent="save">
      <section class="settings-card"><h3>{{ s('Default model', '默认模型') }}</h3><p>{{ loadedCwd || s('Global default', '全局默认') }}</p><label><span>{{ s('Model', '模型') }}</span><select v-model="selected" class="ui-field" data-size="md" :disabled="busy || !connection.supports('provider.select')"><option value="" disabled>{{ s('Automatic fallback — choose a model to pin a default', '自动回退 — 选择模型以固定默认值') }}</option><option v-if="current && !options.some((option) => option.key === current)" :value="current" disabled>{{ snapshot?.providerId }} / {{ snapshot?.modelId }} ({{ s('unavailable', '不可用') }})</option><option v-for="option in options" :key="option.key" :value="option.key">{{ option.label }}</option></select></label><p v-if="!options.length">{{ s('No selectable models. Configure a provider and its model limits first.', '没有可选模型。请先配置提供商和模型限制。') }}</p></section>
      <section v-if="policy" class="settings-card"><h3>{{ s('Subagent models', '子代理模型') }}</h3><p>{{ s('Applies to every newly created native Collaboration member, including named agents. Existing members and external agents are unchanged.', '适用于每个新建的原生协作成员，包括命名代理。现有成员和外部代理不受影响。') }}</p><label><span>{{ s('Model policy', '模型策略') }}</span><select v-model="memberModel" class="ui-field" data-size="md" :disabled="busy || !connection.supports('collaboration.set_model_policy')"><option value="agent">{{ s('Agent decides (profile / workspace defaults)', '由代理决定（配置 / 工作区默认值）') }}</option><option value="inherit">{{ s('Inherit requesting branch model', '继承请求分支的模型') }}</option><option v-if="currentPolicy !== 'agent' && currentPolicy !== 'inherit' && !options.some((option) => option.key === currentPolicy)" :value="currentPolicy" disabled>{{ s('Configured model unavailable', '已配置模型不可用') }}</option><option v-for="option in options" :key="option.key" :value="option.key">{{ option.label }}</option></select></label></section>
      <section v-if="shell" class="settings-card"><h3>{{ s('Command shell', '命令 Shell') }}</h3><p>{{ s('Used for new command invocations. Already running processes keep their shell.', '用于新启动的命令。已运行的进程保留其原 Shell。') }}</p><label><span>{{ s('Executable', '可执行程序') }}</span><select v-model="shellPath" class="ui-field" data-size="md" :disabled="busy || !connection.supports('host.exec_shell.update')"><option value="">{{ s('Automatic detection', '自动检测') }}</option><option v-if="shellPath && !shell.shells.some((choice) => choice.executablePath === shellPath)" :value="shellPath" disabled>{{ shellPath }} ({{ s('unavailable', '不可用') }})</option><option v-for="choice in shell.shells" :key="choice.executablePath" :value="choice.executablePath">{{ choice.name }} — {{ choice.executablePath }}</option></select></label><p v-if="shell.configError" class="ui-banner" data-tone="danger">{{ shell.configError }}</p></section>
      <footer class="settings-actions"><button class="ui-button" data-size="md" data-variant="primary" :disabled="busy || !dirty">{{ s('Save defaults', '保存默认设置') }}</button></footer>
    </form>
  </article>
</template>
