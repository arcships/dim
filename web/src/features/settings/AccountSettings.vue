<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue"
import { useI18n } from "vue-i18n"
import BrandMark from "../../components/BrandMark.vue"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import { copyText } from "../../clipboard"
import { settingsMessages } from "../../i18n/settings"
import { useSettingsText } from "../../i18n/settings-general"
import { readManagement, readReferral, readResetBatches, readUsage, type AccountReferral, type AccountUsage, type ManagementProvider } from "../../rpc/management"
import { readAck } from "../../rpc/read"
import { useConnectionStore } from "../../stores/connection"
import { useModelsStore } from "../../stores/models"
import ProviderAuthFlow from "./ProviderAuthFlow.vue"
import { messageFrom } from "./context"

const { t, locale } = useI18n({ messages: settingsMessages })
const text = useSettingsText()
const connection = useConnectionStore()
const models = useModelsStore()
const provider = ref<ManagementProvider>()
const usage = ref<AccountUsage>()
const referral = ref<AccountReferral>()
const batches = ref<ReturnType<typeof readResetBatches>>()
const referralError = ref("")
const resetError = ref("")
const pendingReset = ref<{ providerId: string; subscriptionId: number; resetType: "window" | "monthly_full"; idempotencyKey: string }>()
const batchesOpen = ref(false)
const copied = ref(false)
const loading = ref(true)
const saving = ref(false)
const error = ref("")
const recoveryProvider = ref<string>()
const usageError = ref("")
const modelRefresh = ref<"refreshed" | { error: string }>()
const confirm = ref<InstanceType<typeof ConfirmDialog>>()
const identity = computed(() => provider.value?.account?.state.identity)
const signedIn = computed(() => provider.value?.account?.state.authenticated === true)
const title = computed(() => identity.value?.name || identity.value?.email || t("account"))
const meters = computed(() => {
  const current = usage.value
  if (!current) return []
  const rows: { id: string; label: string; used?: number; total?: number; remaining?: number; unlimited?: boolean; expires?: string }[] = []
  if (current.credits) rows.push({ id: "credits", label: t("credits"), used: current.credits.usedUnits, total: current.credits.totalUnits, remaining: current.credits.remainingUnits, unlimited: current.credits.unlimited, expires: current.credits.resetsAt })
  for (const w of current.windows) rows.push({ id: `window-${w.durationHours}`, label: `${w.durationHours} h`, used: w.tokenUsed, total: w.tokenCap, expires: w.expiresAt })
  for (const f of current.features) rows.push({ id: f.key, label: f.key, used: f.used, total: f.allowance, remaining: f.remaining, unlimited: f.unlimited, expires: f.periodEnd })
  return rows
})
watch([() => provider.value?.providerId, () => identity.value?.issuer, () => identity.value?.subject, signedIn], () => {
  pendingReset.value = undefined
  usage.value = undefined
  referral.value = undefined
  modelRefresh.value = undefined
}, { flush: "sync" })
async function load(): Promise<void> {
  loading.value = true
  error.value = ""
  recoveryProvider.value = undefined
  usageError.value = ""
  try {
    const view = await connection.client.call("provider.management", {}, readManagement)
    provider.value = view.providers.find((p) => p.credentialAuthority === "account")
    batches.value = undefined
    referralError.value = ""
    if (provider.value?.account?.state.authenticated) {
      const target = { providerId: provider.value.providerId }
      await Promise.all([
        connection.client.call("account.usage", target, readUsage).then((result) => { usage.value = result }, (cause) => { usageError.value = messageFrom(cause) }),
        connection.client.call("account.referral", target, readReferral).then((result) => { referral.value = result }, (cause) => { referralError.value = messageFrom(cause) }),
      ])
    }
    const accountFailure = view.errors.find((e) => e.providerId === "dimcode-api-oauth")
    if (accountFailure) {
      error.value = accountFailure.message
      recoveryProvider.value = accountFailure.providerId
    }
  } catch (cause) { error.value = messageFrom(cause) }
  finally { loading.value = false }
}
async function recover(): Promise<void> {
  if (!recoveryProvider.value || saving.value) return
  saving.value = true
  try {
    await connection.client.call("account.recover", { providerId: recoveryProvider.value }, readAck)
    await Promise.all([load(), models.load()])
  } catch (cause) { error.value = messageFrom(cause) }
  finally { saving.value = false }
}
async function signedInFromAuth(refreshError?: string): Promise<void> {
  await load()
  if (signedIn.value && refreshError) modelRefresh.value = { error: refreshError }
}
async function refreshModels(): Promise<void> {
  const current = provider.value
  if (!current?.actions.includes("refresh") || loading.value || saving.value || connection.phase !== "connected") return
  saving.value = true
  modelRefresh.value = undefined
  try {
    await models.refresh(current.providerId)
    if (signedIn.value) modelRefresh.value = "refreshed"
  } catch (cause) {
    if (signedIn.value) modelRefresh.value = { error: messageFrom(cause) }
  } finally { saving.value = false }
}
async function logout(): Promise<void> {
  const current = provider.value
  if (!current || saving.value || !await confirm.value?.open(t("signOutConfirm"), { confirmLabel: t("signOut") })) return
  saving.value = true
  try {
    await connection.client.call("account.logout", { providerId: current.providerId }, readAck)
    pendingReset.value = undefined
    await Promise.all([load(), models.load()])
  } catch (cause) { error.value = messageFrom(cause) }
  finally { saving.value = false }
}
async function copyInvite(): Promise<void> {
  if (!referral.value?.inviteUrl) return
  try { await copyText(referral.value.inviteUrl); copied.value = true }
  catch (cause) { referralError.value = messageFrom(cause) }
}
async function loadBatches(): Promise<void> {
  if (!provider.value || connection.phase !== "connected") return
  resetError.value = ""
  try { batches.value = await connection.client.call("account.reset_batches", { providerId: provider.value.providerId }, readResetBatches) }
  catch (cause) { resetError.value = messageFrom(cause) }
}
function toggleBatches(open: boolean): void {
  batchesOpen.value = open
  if (open) void loadBatches()
}
async function applyReset(resetType: "window" | "monthly_full"): Promise<void> {
  const subscriptionId = usage.value?.subscriptionId
  if (saving.value || !provider.value || subscriptionId === undefined || (pendingReset.value && pendingReset.value.resetType !== resetType)) return
  const prompt = resetType === "window" ? text("Use one credit to reset the current usage window?", "使用一次额度重置当前用量窗口？") : text("Use one credit to reset this month's full usage?", "使用一次额度重置本月全部用量？")
  if (!await confirm.value?.open(prompt, { confirmLabel: t("reset") })) return
  saving.value = true
  resetError.value = ""
  pendingReset.value ??= { providerId: provider.value.providerId, subscriptionId, resetType, idempotencyKey: crypto.randomUUID() }
  try {
    await connection.client.call("account.apply_reset", pendingReset.value, readAck)
    pendingReset.value = undefined
    await load()
    if (batchesOpen.value) await loadBatches()
  } catch (cause) { resetError.value = `${messageFrom(cause)} ${text("Retry the same reset to safely check its result; no second credit will be consumed.", "请重试同一种重置以安全确认结果；不会重复消耗额度。")}` }
  finally { saving.value = false }
}
watch(() => connection.phase, (phase, before) => { if (phase === "connected" && before !== "connected") void load() })
onMounted(load)
</script>

<template>
  <article class="settings-page">
    <ConfirmDialog ref="confirm" />
    <header><h2 data-route-heading tabindex="-1">{{ t("account") }}</h2><p>{{ t("accountDescription") }}</p></header>
    <UAlert v-if="error" class="settings-banner" color="error" variant="subtle" :description="error" role="alert">
      <template #actions>
        <UButton color="neutral" variant="ghost" size="sm" :disabled="loading || connection.phase !== 'connected'" @click="load">{{ t("retry") }}</UButton>
      </template>
    </UAlert>
    <UButton v-if="recoveryProvider && connection.supports('account.recover')" color="neutral" variant="outline" size="md" :disabled="saving || loading || connection.phase !== 'connected'" @click="recover">{{ text("Recover stored sign-in", "恢复已保存的登录") }}</UButton>
    <UAlert v-if="loading" class="settings-banner" color="neutral" variant="subtle" :description="t('loading')" role="status" />
    <section v-else-if="!provider && !error" class="settings-card"><p class="settings-help">{{ t("accountUnavailable") }}</p></section>
    <template v-if="provider">
      <section class="settings-card">
        <template v-if="signedIn">
          <div class="settings-identity">
            <span class="settings-avatar">{{ title.slice(0, 2).toUpperCase() }}</span>
            <div class="settings-identity-text"><strong>{{ title }}</strong><p v-if="identity?.email">{{ identity.email }}</p><p v-if="identity?.subject">{{ t("uid") }}: {{ identity.subject }}</p></div>
            <UButton v-if="provider.actions.includes('refresh')" color="neutral" variant="outline" size="md" icon="i-lucide-refresh-cw" :disabled="saving || loading || connection.phase !== 'connected'" @click="refreshModels">{{ t("refreshModelList") }}</UButton>
            <UButton color="neutral" variant="ghost" size="md" :disabled="saving || loading || connection.phase !== 'connected'" @click="logout">{{ t("signOut") }}</UButton>
          </div>
          <p v-if="modelRefresh === 'refreshed'" class="settings-help" role="status">{{ t("modelListRefreshed") }}</p>
          <UAlert v-else-if="modelRefresh" class="settings-banner" color="error" variant="subtle" :description="t('modelRefreshFailed', { reason: modelRefresh.error })" role="alert" />
        </template>
        <template v-else>
          <div class="account-login-heading"><BrandMark aria-hidden="true" /><h3>{{ t("signIn") }}</h3></div>
          <ProviderAuthFlow :key="provider.providerId" :provider="provider" @finished="signedInFromAuth" />
        </template>
      </section>
      <section v-if="signedIn" class="settings-card settings-usage">
        <div class="settings-list-toolbar"><h3>{{ t("plan") }}</h3><UButton color="neutral" variant="ghost" size="md" square icon="i-lucide-refresh-cw" :aria-label="t('refresh')" :disabled="loading || connection.phase !== 'connected'" :ui="{ leadingIcon: loading ? 'account-refreshing' : undefined }" @click="load" /></div>
        <UAlert v-if="usageError" color="error" variant="subtle" :description="usageError" role="alert" />
        <div v-if="usage?.plan" class="settings-identity"><div class="settings-identity-text"><strong>{{ usage.plan.name }}</strong><p>{{ usage.plan.status }}<span v-if="usage.plan.expiresAt"> · {{ new Date(usage.plan.expiresAt).toLocaleDateString() }}</span></p><p v-if="usage.plan.priceAmount !== undefined">{{ (usage.plan.priceAmount / 100).toLocaleString(locale, { minimumFractionDigits: 2 }) }} {{ usage.plan.priceCurrency }}<template v-if="usage.plan.priceInterval"> / {{ usage.plan.priceInterval }}</template></p><p v-if="usage.plan.cancelAtPeriodEnd">{{ text("Cancels at the end of this term", "将在本期结束后取消") }}</p><p v-if="usage.plan.lastTermStatus">{{ text("Previous term", "上一周期") }}: {{ usage.plan.lastTermStatus }} · {{ usage.plan.lastTermStart }} — {{ usage.plan.lastTermEnd }}</p></div></div>
        <div v-for="meter in meters" :key="meter.id" class="settings-meter">
          <div class="settings-meter-label"><span>{{ meter.label }}</span><span v-if="meter.unlimited">{{ t("unlimited") }}</span><span v-else><template v-if="meter.used !== undefined">{{ meter.used.toLocaleString() }}</template><template v-if="meter.total !== undefined"> / {{ meter.total.toLocaleString() }}</template><template v-if="meter.remaining !== undefined"> · {{ t("remaining") }} {{ meter.remaining.toLocaleString() }}</template></span></div>
          <UProgress v-if="!meter.unlimited && meter.total !== undefined && meter.total > 0 && meter.used !== undefined" :model-value="meter.used" :max="meter.total" :aria-label="meter.label" size="sm" />
          <small v-if="meter.expires" class="settings-meta">{{ t("expires") }}: {{ new Date(meter.expires).toLocaleString() }}</small>
        </div>
        <div class="settings-inline-actions"><UButton v-if="usage?.subscriptionUrl" color="neutral" variant="outline" size="md" trailing-icon="i-lucide-external-link" :href="usage.subscriptionUrl" target="_blank" rel="noopener noreferrer">{{ t("viewPlan") }}</UButton><UButton v-if="usage?.pricingUrl" color="neutral" variant="ghost" size="md" trailing-icon="i-lucide-external-link" :href="usage.pricingUrl" target="_blank" rel="noopener noreferrer">{{ t("choosePlan") }}</UButton></div>
      </section>
      <section v-if="signedIn && usage?.resets" class="settings-card settings-form">
        <h3>{{ t("resetCredits") }}</h3>
        <UAlert v-if="resetError" color="error" variant="subtle" :description="resetError" role="alert" />
        <div v-for="kind in (['window', 'monthlyFull'] as const)" :key="kind" class="settings-row">
          <div class="settings-row-main"><strong>{{ kind === 'window' ? text('Window reset', '窗口重置') : text('Monthly full reset', '每月全量重置') }} · {{ usage.resets[kind].availableCount }}</strong><small v-if="usage.resets[kind].nearestExpiresAt">{{ t("expires") }}: {{ new Date(usage.resets[kind].nearestExpiresAt!).toLocaleString() }}</small></div>
          <UButton color="neutral" variant="outline" size="md" :disabled="saving || loading || connection.phase !== 'connected' || (pendingReset ? pendingReset.resetType !== (kind === 'window' ? 'window' : 'monthly_full') : !usage.resets[kind].availableCount) || usage.subscriptionId === undefined" @click="applyReset(kind === 'window' ? 'window' : 'monthly_full')">{{ t("reset") }}</UButton>
        </div>
        <UCollapsible :open="batchesOpen" @update:open="toggleBatches">
          <UButton color="neutral" variant="link" size="sm" class="px-0" :trailing-icon="batchesOpen ? 'i-lucide-chevron-down' : 'i-lucide-chevron-right'">{{ text("Credit batches and expiry", "重置额度批次与有效期") }}</UButton>
          <template #content>
            <template v-if="batches"><div v-for="kind in (['window', 'monthlyFull'] as const)" :key="kind"><h4>{{ kind === 'window' ? text('Window', '窗口') : text('Monthly', '每月') }}</h4><p v-for="batch in batches[kind]" :key="batch.grantId" class="settings-help">{{ batch.sourceType }} · {{ batch.remaining }} · {{ batch.scopeType }} {{ batch.subscriptionId }} · {{ new Date(batch.expiresAt).toLocaleString() }}</p><p v-if="!batches[kind].length" class="settings-help">{{ text("No credits", "暂无额度") }}</p></div></template>
          </template>
        </UCollapsible>
      </section>
      <section v-if="signedIn && (referral?.enabled || referralError)" class="settings-card settings-form">
        <h3>{{ t("referral") }}</h3><UAlert v-if="referralError" color="error" variant="subtle" :description="referralError" role="alert" />
        <template v-if="referral?.enabled"><p>{{ text("Invited", "已邀请") }} {{ referral.invitedCount }} · {{ text("Subscribed", "已付费") }} {{ referral.paidCount }} · {{ text("Rewards", "奖励额度") }} {{ referral.rewardGrantedUnits ?? '—' }}</p><p class="settings-help">{{ text("Your rebate", "邀请人返利") }} {{ referral.inviterRebateBp / 100 }}% · {{ text("Friend's discount", "受邀人优惠") }} {{ referral.inviteeRebateBp / 100 }}%</p><template v-if="referral.eligible && referral.inviteUrl"><UFormField :label="text('Invitation link', '邀请链接')"><UInput :model-value="referral.inviteUrl" size="md" readonly class="w-full" /></UFormField><div class="settings-inline-actions"><UButton color="neutral" variant="outline" size="md" @click="copyInvite">{{ t(copied ? 'copied' : 'copy') }}</UButton><UButton color="neutral" variant="ghost" size="md" trailing-icon="i-lucide-external-link" :href="referral.inviteUrl" target="_blank" rel="noopener noreferrer">{{ text("Open", "打开") }}</UButton></div></template><p v-else class="settings-help">{{ text("Your account is not eligible for invitations yet.", "当前账户暂未获得邀请资格。") }}</p></template>
      </section>
    </template>
  </article>
</template>

<style scoped>
.account-login-heading { display: flex; align-items: center; gap: var(--space-3); margin-bottom: var(--space-6); }
.account-login-heading svg { width: 36px; height: 26px; }
.account-login-heading h3, .settings-list-toolbar h3 { margin: 0; }
:deep(.account-refreshing) { animation: spin 1s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) { :deep(.account-refreshing) { animation: none; } }
</style>
