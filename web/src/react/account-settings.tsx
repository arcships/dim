import { Button } from "@heroui/react"
import { useEffect, useRef, useState } from "react"
import { copyText } from "../clipboard"
import {
  readManagementPage, readReferral, readResetBatches, readUsage,
  type AccountReferral, type AccountUsage, type ManagementInfo,
} from "../rpc/management"
import { readAck } from "../rpc/read"
import { ProviderAuth } from "./provider-auth"
import { connection, useStoreValue } from "./stores"

const readOnline = () => connection.phase === "connected"
const providerErrorId = "dimcode-api-oauth"
type Text = (en: string, zh: string) => string
type Reset = { providerId: string; subscriptionId: number; resetType: "window" | "monthly_full"; idempotencyKey: string }
type Meter = { id: string; label: string; used?: number; total?: number; remaining?: number; unlimited?: boolean; expires?: string; unit?: string }
const formatTime = (value: string) => new Date(value).toLocaleString()
const errorMessage = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)

export function AccountSettings({ text, onDirty }: { text: Text; onDirty: (dirty: boolean) => void }) {
  const online = useStoreValue(readOnline)
  const [provider, setProvider] = useState<ManagementInfo>()
  const [usage, setUsage] = useState<AccountUsage>()
  const [referral, setReferral] = useState<AccountReferral>()
  const [batches, setBatches] = useState<ReturnType<typeof readResetBatches>>()
  const [batchesOpen, setBatchesOpen] = useState(false)
  const [batchesLoading, setBatchesLoading] = useState(false)
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [usageError, setUsageError] = useState("")
  const [referralError, setReferralError] = useState("")
  const [batchError, setBatchError] = useState("")
  const [resetError, setResetError] = useState("")
  const [refreshError, setRefreshError] = useState("")
  const [notice, setNotice] = useState("")
  const [recovery, setRecovery] = useState<string>()
  const [pendingReset, setPendingReset] = useState<Reset>()
  const [inviteCopied, setInviteCopied] = useState(false)
  const request = useRef(0)
  const previousAccount = useRef("")
  const batchRequest = useRef(0)
  const identity = provider?.account?.state.identity
  const signedIn = provider?.account?.state.authenticated === true
  const permitted = (method: string) => online && !busy && !loading && connection.supports(method)

  async function load() {
    if (!online || !connection.supports("provider.management.list")) return false
    const id = ++request.current
    setLoading(true)
    setError("")
    setUsageError("")
    setReferralError("")
    setRecovery(undefined)
    try {
      const view = await connection.client.call("provider.management.list", { accountsOnly: true, limit: 128 }, readManagementPage)
      if (id !== request.current) return false
      const account = view.providers.find((row) => row.credentialAuthority === "account")
      const state = account?.account?.state
      const fingerprint = JSON.stringify([account?.providerId, state?.authenticated, state?.identity?.issuer, state?.identity?.subject, state?.preview])
      if (fingerprint !== previousAccount.current) {
        previousAccount.current = fingerprint
        batchRequest.current++
        setPendingReset(undefined)
        setBatches(undefined)
        setBatchesOpen(false)
        setBatchesLoading(false)
        setUsage(undefined)
        setReferral(undefined)
        setInviteCopied(false)
        setNotice("")
        setResetError("")
        setBatchError("")
        setRefreshError("")
      }
      setProvider(account)
      const failure = view.errors.find((item) => item.providerId === (account?.providerId ?? providerErrorId))
      if (failure) {
        setError(failure.message)
        setRecovery(failure.providerId)
      } else if (state?.recoveryRequired) setRecovery(account?.providerId)
      if (state?.authenticated && account) {
        const params = { providerId: account.providerId }
        await Promise.all([
          connection.supports("account.usage") ? connection.client.call("account.usage", params, readUsage).then((usage) => {
            if (id === request.current) setUsage(usage)
          }, (cause) => { if (id === request.current) setUsageError(errorMessage(cause)) }) : undefined,
          connection.supports("account.referral") ? connection.client.call("account.referral", params, readReferral).then((referral) => {
            if (id === request.current) setReferral(referral)
          }, (cause) => { if (id === request.current) setReferralError(errorMessage(cause)) }) : undefined,
        ])
      }
      return id === request.current
    } catch (cause) {
      if (id === request.current) setError(errorMessage(cause))
      return false
    } finally {
      if (id === request.current) setLoading(false)
    }
  }

  useEffect(() => {
    if (online) void load()
    return () => { request.current++; batchRequest.current++ }
  }, [online])

  async function accountAction(method: "account.logout" | "account.recover" | "provider.refresh", providerId: string) {
    if (!permitted(method)) return
    if (method === "account.logout" && !window.confirm(text("Sign out of this account?", "退出此账户？"))) return
    setBusy(true)
    setNotice("")
    setError("")
    setRefreshError("")
    try {
      await connection.client.call(method, { providerId }, readAck)
      if (method === "provider.refresh") setNotice(text("Model list refreshed.", "模型列表已刷新。"))
      else await load()
    } catch (cause) {
      if (method === "provider.refresh") setRefreshError(errorMessage(cause))
      else setError(errorMessage(cause))
    } finally {
      setBusy(false)
    }
  }

  async function loadBatches(providerId: string) {
    if (!online || !connection.supports("account.reset_batches")) return
    const id = ++batchRequest.current
    setBatchError("")
    setBatchesLoading(true)
    try {
      const result = await connection.client.call("account.reset_batches", { providerId }, readResetBatches)
      if (id === batchRequest.current) setBatches(result)
    } catch (cause) {
      if (id === batchRequest.current) setBatchError(errorMessage(cause))
    } finally {
      if (id === batchRequest.current) setBatchesLoading(false)
    }
  }

  async function applyReset(resetType: Reset["resetType"]) {
    const subscriptionId = usage?.subscriptionId
    if (!provider || subscriptionId === undefined || !permitted("account.apply_reset") || (pendingReset && pendingReset.resetType !== resetType)) return
    const message = resetType === "window" ? text("Use one credit to reset the current usage window?", "使用一次额度重置当前用量窗口？")
      : text("Use one credit to reset this month's full usage?", "使用一次额度重置本月全部用量？")
    if (!window.confirm(message)) return
    const reset = pendingReset ?? { providerId: provider.providerId, subscriptionId, resetType, idempotencyKey: crypto.randomUUID() }
    setPendingReset(reset)
    setBusy(true)
    setResetError("")
    try {
      await connection.client.call("account.apply_reset", reset, readAck)
      setPendingReset(undefined)
      await load()
      if (batchesOpen) await loadBatches(reset.providerId)
    } catch (cause) {
      setResetError(`${errorMessage(cause)} ${text("Retry the same reset to check its result without consuming another credit.", "重试同一种重置可确认结果，不会重复消耗额度。")}`)
    } finally {
      setBusy(false)
    }
  }

  const meters: Meter[] = usage ? [
    ...(usage.credits ? [{
      id: "credits", label: text("Credits", "额度"), used: usage.credits.usedUnits, total: usage.credits.totalUnits,
      remaining: usage.credits.remainingUnits, unlimited: usage.credits.unlimited, expires: usage.credits.resetsAt,
    }] : []),
    ...usage.windows.map(({ durationHours, tokenUsed, tokenCap, expiresAt }) => ({
      id: `window-${durationHours}`, label: `${durationHours} h`, used: tokenUsed, total: tokenCap, expires: expiresAt,
    })),
    ...usage.features.map(({ key, used, allowance, remaining, unlimited, periodEnd, unit }) => ({
      id: key, label: key, used, total: allowance, remaining, unlimited, expires: periodEnd, unit,
    })),
  ] : []

  return <article className="rw-settings">
    <h1>{text("Account", "账户")}</h1>
    {!online && <p role="status">{text("Disconnected.", "连接已断开。")}</p>}
    {loading && <p role="status">{text("Loading account…", "正在加载账户…")}</p>}
    {error && <p className="rw-error" role="alert">{error}</p>}
    {notice && <p role="status">{notice}</p>}
    <div className="rw-setting-options">
      <Button size="sm" variant="secondary" isDisabled={!permitted("provider.management.list")} onPress={() => void load()}>
        {text("Refresh account", "刷新账户")}
      </Button>
      {recovery && <Button size="sm" variant="secondary" isDisabled={!permitted("account.recover")}
        onPress={() => void accountAction("account.recover", recovery)}>{text("Recover stored sign-in", "恢复已保存的登录")}</Button>}
    </div>
    {!provider && !loading && !error && online && <p>{text("Dim account is unavailable on this connection.", "当前连接无法使用 Dim 账户。")}</p>}
    {provider?.account && <section className="rw-settings-card">
      <h2>{identity?.name || identity?.email || provider.displayName}</h2>
      {signedIn ? <>
        {identity?.name && identity.email && <p>{identity.email}</p>}
        {(identity?.subject || provider.account.state.preview) && <p>{identity?.subject ?? provider.account.state.preview}</p>}
        {refreshError && <p role="alert" className="rw-error">{text("Model list refresh failed", "刷新模型列表失败")} · {refreshError}</p>}
        <div className="rw-setting-options">
          {provider.actions.includes("refresh") && <Button size="sm" variant="secondary" isDisabled={!permitted("provider.refresh")}
            onPress={() => void accountAction("provider.refresh", provider.providerId)}>{text("Refresh model list", "刷新模型列表")}</Button>}
          <Button size="sm" variant="secondary" isDisabled={!permitted("account.logout")}
            onPress={() => void accountAction("account.logout", provider.providerId)}>{text("Sign out", "退出登录")}</Button>
        </div>
      </> : <ProviderAuth key={provider.providerId} provider={{ ...provider, account: provider.account }} text={text} onDirty={onDirty}
        onFinished={async (refreshError) => { await load(); setRefreshError(refreshError ?? "") }} />}
    </section>}
    {signedIn && <section className="rw-settings-card">
      <h2>{text("Plan and usage", "套餐与用量")}</h2>
      {usageError && <p className="rw-error" role="alert">{usageError}</p>}
      {usage?.plan && <>
        <p><strong>{usage.plan.name}</strong>{usage.plan.status && <> · {usage.plan.status}</>}</p>
        {usage.plan.priceAmount !== undefined && <p>
          {(usage.plan.priceAmount / 100).toLocaleString([], { minimumFractionDigits: 2 })} {usage.plan.priceCurrency}
          {usage.plan.priceInterval && <> / {usage.plan.priceInterval}</>}
        </p>}
        {usage.plan.expiresAt && <p>{text("Expires", "过期时间")} · {formatTime(usage.plan.expiresAt)}</p>}
        {usage.plan.cancelAtPeriodEnd && <p>{text("Cancels at the end of this term.", "将在本期结束后取消。")}</p>}
        {usage.plan.lastTermStatus && <p>{text("Previous term", "上一周期")} · {usage.plan.lastTermStatus}
          {usage.plan.lastTermStart && <> · {formatTime(usage.plan.lastTermStart)}</>}{usage.plan.lastTermEnd && <> – {formatTime(usage.plan.lastTermEnd)}</>}</p>}
      </>}
      {meters.map(({ id, label, used, total, remaining, unlimited, expires, unit }) => <div className="rw-account-meter" key={id}>
        <strong>{label}</strong>
        <span>{unlimited ? text("Unlimited", "无限制") : [used?.toLocaleString(), total?.toLocaleString()].filter(Boolean).join(" / ")}
          {!unlimited && unit && <> {unit}</>}{remaining !== undefined && !unlimited && <> · {text("Remaining", "剩余")} {remaining.toLocaleString()}</>}</span>
        {!unlimited && total !== undefined && total > 0 && used !== undefined && <progress max={total} value={used} aria-label={label} />}
        {expires && <small className="rw-muted">{text("Expires", "过期时间")} · {formatTime(expires)}</small>}
      </div>)}
      <div className="rw-setting-options">
        {usage?.subscriptionUrl && <a className="rw-settings-link" target="_blank" rel="noopener noreferrer" href={usage.subscriptionUrl}>{text("View plan", "查看套餐")}</a>}
        {usage?.pricingUrl && <a className="rw-settings-link" target="_blank" rel="noopener noreferrer" href={usage.pricingUrl}>{text("View pricing", "查看价格")}</a>}
      </div>
    </section>}
    {signedIn && usage?.resets && provider && <section className="rw-settings-card">
      <h2>{text("Reset credits", "重置额度")}</h2>
      {resetError && <p role="alert" className="rw-error">{resetError}</p>}
      {(["window", "monthlyFull"] as const).map((kind) => {
        const type = kind === "window" ? "window" : "monthly_full"
        const balance = usage.resets![kind]
        const label = kind === "window" ? text("Window reset", "窗口重置") : text("Monthly full reset", "每月全量重置")
        return <div className="rw-account-meter" key={kind}>
          <strong>{label}</strong><span>{balance.availableCount}</span>
          {balance.nearestExpiresAt && <small>{text("Expires", "过期时间")} · {formatTime(balance.nearestExpiresAt)}</small>}
          <Button size="sm" variant="secondary" isDisabled={!permitted("account.apply_reset") || usage.subscriptionId === undefined
            || (pendingReset ? pendingReset.resetType !== type : balance.availableCount === 0)} onPress={() => void applyReset(type)}>
            {pendingReset?.resetType === type ? text("Retry reset", "重试重置") : label}
          </Button>
        </div>
      })}
      {connection.supports("account.reset_batches") && <details className="rw-provider-options" onToggle={(event) => {
        const open = event.currentTarget.open
        setBatchesOpen(open)
        if (open) void loadBatches(provider.providerId)
      }}>
        <summary>{text("Credit batches and expiry", "重置额度批次与有效期")}</summary>
        {batchesLoading && <p role="status">{text("Loading credit batches…", "正在加载额度批次…")}</p>}
        {batchError && <p role="alert" className="rw-error">{batchError}</p>}
        {batches && (["window", "monthlyFull"] as const).map((kind) => <div key={kind}>
          <h3>{kind === "window" ? text("Window", "窗口") : text("Monthly", "每月")}</h3>
          {batches[kind].length === 0 && <p>{text("No credits", "暂无额度")}</p>}
          {batches[kind].map((batch) => <p key={batch.grantId}>{batch.sourceType} · {batch.remaining} · {batch.scopeType} {batch.subscriptionId} · {formatTime(batch.expiresAt)}</p>)}
        </div>)}
      </details>}
    </section>}
    {signedIn && (referral?.enabled || referralError) && <section className="rw-settings-card rw-settings-fields">
      <h2>{text("Referrals", "邀请")}</h2>
      {referralError && <p className="rw-error" role="alert">{referralError}</p>}
      {referral?.enabled && <>
        <p>{text("Invited", "已邀请")} {referral.invitedCount} · {text("Subscribed", "已付费")} {referral.paidCount} · {text("Rewards", "奖励额度")} {referral.rewardGrantedUnits ?? "—"}</p>
        <p>{text("Your rebate", "邀请人返利")} {referral.inviterRebateBp / 100}% · {text("Friend's discount", "受邀人优惠")} {referral.inviteeRebateBp / 100}%</p>
        {referral.eligible && referral.inviteUrl ? <>
          <label>{text("Invitation link", "邀请链接")}<input readOnly value={referral.inviteUrl} /></label>
          <div className="rw-setting-options">
            <Button size="sm" variant="secondary" onPress={async () => {
              try { await copyText(referral.inviteUrl!); setInviteCopied(true) }
              catch (cause) { setReferralError(errorMessage(cause)) }
            }}>{inviteCopied ? text("Copied", "已复制") : text("Copy link", "复制链接")}</Button>
            <a className="rw-settings-link" href={referral.inviteUrl} target="_blank" rel="noopener noreferrer">{text("Open invitation", "打开邀请")}</a>
          </div>
        </> : <p>{text("This account is not eligible for invitations yet.", "此账户暂未获得邀请资格。")}</p>}
      </>}
    </section>}
  </article>
}
