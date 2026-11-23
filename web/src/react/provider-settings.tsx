import { Button } from "@heroui/react"
import { useEffect, useLayoutEffect, useRef, useState } from "react"
import { readManagementPage, type ManagementInfo, type ManagementPage } from "../rpc/management"
import { readAck } from "../rpc/read"
import { ProviderAuth } from "./provider-auth"
import { ProviderConnection } from "./provider-connection"
import { ProviderModels } from "./provider-models"
import { connection, useStoreValue } from "./stores"

const base = "/settings/providers"
const readOnline = () => connection.phase === "connected"
type Text = (en: string, zh: string) => string

function configured(provider: ManagementInfo) {
  return provider.source === "custom" || provider.account?.state.authenticated === true
    || provider.connection?.credentialConfigured === true || (!provider.account && provider.enabled === true)
}

export function ProviderSettings({ detail, onDirty, open, text }: {
  detail?: string
  onDirty: (dirty: boolean) => void
  open: (path: string, replace?: boolean, saved?: boolean) => void
  text: Text
}) {
  const online = useStoreValue(readOnline)
  const [query, setQuery] = useState("")
  const [page, setPage] = useState<ManagementPage>()
  const [provider, setProvider] = useState<ManagementInfo>()
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [notice, setNotice] = useState("")
  const [formKey, setFormKey] = useState(0)
  const [custom, setCustom] = useState(false)
  const [tab, setTab] = useState<"connection" | "models">("connection")
  const [formDirty, markDirty] = useState(false)
  const [authDirty, setAuthDirty] = useState(false)
  const dirty = formDirty || authDirty
  const request = useRef(0)
  const add = detail === "new"
  const providerId = add ? undefined : detail
  const supported = online && connection.supports("provider.management.list")

  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  async function load(more = false) {
    if (!supported) return
    const read = ++request.current
    setLoading(true)
    setError("")
    try {
      const result = await connection.client.call("provider.management.list", providerId ? { providerId }
        : { query, ...(more && page?.next ? { after: page.next } : {}) }, readManagementPage)
      if (read !== request.current) return
      if (providerId) {
        const current = result.providers.find((item) => item.providerId === providerId)
        setProvider(current)
        if (!current) setError(result.errors.find((entry) => entry.providerId === providerId)?.message
          ?? text("Provider unavailable.", "供应商不可用。"))
      }
      setPage((previous) => more && previous ? {
        ...result,
        providers: [...previous.providers, ...result.providers],
        errors: [...previous.errors, ...result.errors],
      } : result)
      return true
    } catch (cause) {
      if (read === request.current) setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (read === request.current) setLoading(false)
    }
  }

  useEffect(() => {
    setCustom(false)
    setTab("connection")
    setProvider(undefined)
    setPage(undefined)
    setError("")
    setFormKey((key) => key + 1)
  }, [detail])

  useEffect(() => {
    if (supported && !dirty) {
      const timer = window.setTimeout(() => void load(), providerId ? 0 : 150)
      return () => {
        clearTimeout(timer)
        request.current++
      }
    }
  }, [supported, detail, query])

  function reload() {
    if (dirty && !window.confirm(text("Discard unsaved provider changes?", "放弃未保存的供应商更改？"))) return
    void load().then((loaded) => { if (loaded) setFormKey((key) => key + 1) })
  }

  function saved(result: ManagementInfo) {
    markDirty(false)
    setNotice(text(`Saved ${result.displayName}.`, `已保存 ${result.displayName}。`))
    open(base, true, true)
  }

  async function action(kind: "refresh" | "delete" | "logout", current: ManagementInfo) {
    if (!online || busy || dirty || !current.actions.includes(kind)) return
    const method = kind === "logout" ? `${current.account?.namespace}.logout` : `provider.${kind}`
    if (!connection.supports(method)) return
    if (kind === "delete" && !window.confirm(text(`Remove ${current.displayName}?`, `移除 ${current.displayName}？`))) return
    if (kind === "logout" && !window.confirm(text(`Sign out of ${current.displayName}?`, `退出 ${current.displayName}？`))) return
    setBusy(true)
    setError("")
    setNotice("")
    try {
      await connection.client.call(method, kind === "delete" ? { providerId: current.providerId, expectedVersion: current.version }
        : kind === "refresh" || current.account?.namespace === "account" ? { providerId: current.providerId } : {}, readAck)
      if (kind === "delete") open(base, true)
      else {
        const loaded = await load()
        if (kind === "refresh" && loaded) {
          setFormKey((key) => key + 1)
          setNotice(text("Model list refreshed.", "模型列表已刷新。"))
        }
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const list = page?.providers.filter((provider) => configured(provider) !== add
    && `${provider.displayName} ${provider.providerId}`.toLowerCase().includes(query.trim().toLowerCase()))
  return <article className="rw-settings">
    {detail && <Button className="rw-settings-reload" size="sm" variant="tertiary" onPress={() => open(base)}>{text("Back to providers", "返回供应商列表")}</Button>}
    <h1>{add ? text("Add provider", "添加供应商") : provider?.displayName ?? text("Providers", "供应商")}</h1>
    {!online && <p role="status">{text("Disconnected. Changes cannot be saved.", "连接已断开，无法保存更改。")}</p>}
    {online && !supported && <p role="status">{text("Provider management is unavailable on this connection.", "当前连接无法管理供应商。")}</p>}
    {loading && <p role="status">{text("Loading providers…", "正在加载供应商…")}</p>}
    {notice && <p role="status">{notice}</p>}
    {error && <div className="rw-settings-card">
      <p role="alert" className="rw-error">{error}</p>
      <Button size="sm" variant="secondary" isDisabled={!supported || busy || loading} onPress={reload}>{text("Retry", "重试")}</Button>
    </div>}
    {!providerId && <>
      {!custom && <div className="rw-field-row">
        <label className="rw-setting-language">{text("Search providers", "搜索供应商")}
          <input type="search" value={query} onChange={(event) => setQuery(event.target.value)} />
        </label>
        {!add && <Button size="sm" isDisabled={!supported} onPress={() => open(`${base}/new`)}>{text("Add provider", "添加供应商")}</Button>}
        {add && <Button size="sm" isDisabled={!supported || !page?.drivers.length || !connection.supports("provider.create")}
          onPress={() => setCustom(true)}>{text("Add custom provider", "添加自定义供应商")}</Button>}
      </div>}
      {!custom && page?.errors.map(({ providerId, message }, index) => <p className="rw-error" key={`${providerId}/${index}`}>{providerId} · {message}</p>)}
      {custom && page ? <ProviderConnection key={formKey} drivers={page.drivers} online={online} text={text} onDirty={markDirty}
        onSaved={saved} onReload={reload} onCancel={() => open(base)} /> : <>
        {list?.map((row) => <section className="rw-settings-card" key={row.providerId} aria-label={row.displayName}>
          <Button size="sm" variant="tertiary" onPress={() => open(`${base}/${encodeURIComponent(row.providerId)}`)}>{row.displayName}</Button>
          {row.connection?.baseUrl && <p>{row.connection.baseUrl}</p>}
          <p>{row.account && <>OAuth · </>}{row.enabled === false ? text("Disabled", "已禁用") : configured(row) ? text("Configured", "已配置") : text("Available", "可用")}</p>
        </section>)}
        {list?.length === 0 && <p>{query ? text("No matching providers.", "没有匹配的供应商。")
          : add ? text("No additional providers.", "没有其他供应商。") : text("No configured providers.", "没有已配置的供应商。")}</p>}
        {page?.next && <Button className="rw-settings-reload" size="sm" variant="secondary" isDisabled={!supported || loading}
          onPress={() => void load(true)}>{text("Load more providers", "加载更多供应商")}</Button>}
      </>}
    </>}
    {provider && page && <>
      <div className="rw-setting-options">
        {(["connection", "models"] as const).map((view) => <Button key={view} size="sm"
          variant={tab === view ? "primary" : "secondary"} aria-pressed={tab === view}
          onPress={() => {
            if (view === tab || (dirty && !window.confirm(text("Discard unsaved changes?", "放弃未保存的更改？")))) return
            setTab(view)
          }}>{view === "connection" ? text("Connection", "连接") : text("Models", "模型")}</Button>)}
        {provider.actions.includes("refresh") && <Button size="sm" variant="secondary" isDisabled={!online || busy || dirty || !connection.supports("provider.refresh")}
          onPress={() => void action("refresh", provider)}>{text("Refresh model list", "刷新模型列表")}</Button>}
        {provider.actions.includes("delete") && <Button size="sm" variant="danger-soft" isDisabled={!online || busy || dirty || !connection.supports("provider.delete")}
          onPress={() => void action("delete", provider)}>{text("Remove provider", "移除供应商")}</Button>}
      </div>
      {tab === "models" && <ProviderModels key={provider.providerId} providerId={provider.providerId} online={online && !busy}
        refreshCount={formKey} text={text} onDirty={markDirty}
        onVersion={(version) => setProvider((current) => current?.providerId === provider.providerId ? { ...current, version } : current)} />}
      {tab === "connection" && provider.account && <section className="rw-settings-card">
        <h2>{text("Account", "账户")}</h2>
        <p>{provider.account.state.authenticated
          ? provider.account.state.identity?.email ?? provider.account.state.preview ?? text("Signed in", "已登录")
          : text("Signed out", "未登录")}</p>
        {!provider.account.state.authenticated && provider.actions.includes("login") && <ProviderAuth
          key={provider.providerId} provider={{ ...provider, account: provider.account }} text={text} onDirty={setAuthDirty}
          onFinished={async (refreshError) => {
            await load()
            if (refreshError) setError(text(`Signed in. Model list refresh failed: ${refreshError}`, `已登录，刷新模型列表失败：${refreshError}`))
          }} />}
        {provider.account.state.authenticated && provider.actions.includes("logout") && <Button size="sm" variant="secondary"
          isDisabled={busy || !online || dirty || !connection.supports(`${provider.account.namespace}.logout`)}
          onPress={() => void action("logout", provider)}>{text("Sign out", "退出登录")}</Button>}
      </section>}
      {tab === "connection" && provider.connection && <ProviderConnection key={`${provider.providerId}:${provider.version}:${formKey}`} provider={provider} drivers={page.drivers} online={online && !busy}
        text={text} onDirty={markDirty} onSaved={saved} onReload={reload} onCancel={() => open(base)} />}
    </>}
  </article>
}
