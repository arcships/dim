import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import { useEffect, useLayoutEffect, useRef, useState, type FormEvent } from "react"
import { copyText } from "../clipboard"
import { readAccountDescriptors, readLoginAttempt, readLoginStatus, type LoginAttempt, type ManagementInfo } from "../rpc/management"
import { readAck } from "../rpc/read"
import { connection, useStoreValue } from "./stores"

const readOnline = () => connection.phase === "connected"
const regions = ["https://dimagent.cn", "https://dimagent.com"]
type ActiveAttempt = LoginAttempt & { mode: "browser" | "device" }
type Text = (en: string, zh: string) => string

export function ProviderAuth({ provider, text, onFinished, onDirty }: {
  provider: ManagementInfo & Required<Pick<ManagementInfo, "account">>
  text: Text
  onFinished: (refreshError?: string) => void | Promise<void>
  onDirty: (dirty: boolean) => void
}) {
  const online = useStoreValue(readOnline)
  const { namespace, loginMethods } = provider.account
  const managed = namespace === "account"
  const providerId = provider.providerId
  const target = managed ? { providerId } : {}
  const [mode, setMode] = useState<"browser" | "device">(loginMethods.includes("browser") ? "browser" : "device")
  const [directory, setDirectory] = useState("")
  const [enterprise, setEnterprise] = useState(false)
  const [email, setEmail] = useState("")
  const [callback, setCallback] = useState("")
  const [attempt, setAttempt] = useState<ActiveAttempt>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [copyStatus, setCopyStatus] = useState("")
  const [pollKey, setPollKey] = useState(0)
  const attemptRef = useRef<ActiveAttempt | undefined>(undefined)
  const mounted = useRef(false)
  const finished = useRef(onFinished)
  finished.current = onFinished
  const url = attempt?.authorizationUrl ?? attempt?.verificationUriComplete ?? attempt?.verificationUri
  const beginMethod = `${namespace}.login.${mode === "browser" ? "browser.begin" : "begin"}`
  const completeMethod = `${namespace}.login.browser.complete`
  const dirty = Boolean(attempt || email || callback)

  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty, onDirty])

  function cancelRequest(current: LoginAttempt) {
    return connection.client.call(`${namespace}.login.cancel`, { ...target, attemptId: current.attemptId }, readAck)
  }

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      if (attemptRef.current) void cancelRequest(attemptRef.current).catch(() => {})
    }
  }, [])

  useEffect(() => {
    if (!managed || !online || !connection.supports("account.providers")) return
    let active = true
    void connection.client.call("account.providers", {}, readAccountDescriptors).then((providers) => {
      const base = providers.find((entry) => entry.providerId === providerId)?.defaultDirectory
      if (active && base) setDirectory((value) => value || base)
    }, (cause) => {
      if (active) setError(cause instanceof Error ? cause.message : String(cause))
    })
    return () => { active = false }
  }, [online, managed, providerId])

  useEffect(() => {
    if (!attempt || !online || busy) return
    let active = true
    let timer = window.setTimeout(() => void poll(), 100)
    async function poll() {
      if (!attempt || connection.phase !== "connected") return
      try {
        const status = await connection.client.call(`${namespace}.login.status`, { ...target, attemptId: attempt.attemptId }, readLoginStatus)
        if (!active || attemptRef.current !== attempt) return
        if (status === "pending") {
          if (Date.now() >= attempt.expiresAt) {
            attemptRef.current = undefined
            setAttempt(undefined)
            setCallback("")
            setError(text("Sign-in attempt expired.", "登录尝试已过期。"))
            void cancelRequest(attempt).catch(() => {})
          } else {
            timer = window.setTimeout(() => void poll(), 1500)
          }
          return
        }
        attemptRef.current = undefined
        setAttempt(undefined)
        setCallback("")
        if (status !== "completed") {
          setError(status === "expired" ? text("Sign-in attempt expired.", "登录尝试已过期。")
            : status === "cancelled" ? text("Sign-in cancelled.", "登录已取消。") : text("Sign-in failed.", "登录失败。"))
          return
        }
        setBusy(true)
        setEmail("")
        setError("")
        let refreshError: string | undefined
        if (managed && connection.supports("provider.refresh")) {
          try { await connection.client.call("provider.refresh", { providerId }, readAck) }
          catch (cause) { refreshError = cause instanceof Error ? cause.message : String(cause) }
        }
        if (mounted.current) await finished.current(refreshError)
      } catch (cause) {
        if (active || (mounted.current && attemptRef.current === undefined)) {
          setError(cause instanceof Error ? cause.message : String(cause))
        }
      } finally {
        if (mounted.current && attemptRef.current === undefined) setBusy(false)
      }
    }
    return () => {
      active = false
      clearTimeout(timer)
    }
  }, [attempt, online, busy, pollKey])

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!online || busy) return
    if (attempt && (attempt.mode !== "browser" || !callback.trim() || !connection.supports(completeMethod))) return
    if (!attempt && (!loginMethods.includes(mode) || !connection.supports(beginMethod) || (managed && !directory))) return
    setError("")
    setCopyStatus("")
    setBusy(true)
    try {
      if (attempt) {
        await connection.client.call(completeMethod, { ...target, attemptId: attempt.attemptId, callbackUrl: callback.trim() }, readAck)
      } else {
        const result = await connection.client.call(beginMethod, {
          ...target,
          ...(managed ? { directory, ...(enterprise ? { email } : {}),
            ...(mode === "browser" ? { redirectUri: "http://localhost:54321/auth/callback" } : {}) } : {}),
        }, readLoginAttempt)
        const next = { ...result, mode }
        if (!mounted.current) {
          await cancelRequest(next)
          return
        }
        attemptRef.current = next
        setAttempt(next)
      }
    } catch (cause) {
      if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (mounted.current) {
        setCallback("")
        setBusy(false)
      }
    }
  }

  async function cancel() {
    if (!attempt || busy) return
    setBusy(true)
    setError("")
    setCopyStatus("")
    try {
      await cancelRequest(attempt)
      if (attemptRef.current === attempt) {
        attemptRef.current = undefined
        setAttempt(undefined)
        setCallback("")
      }
    } catch (cause) {
      if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (mounted.current) setBusy(false)
    }
  }

  return <form className="rw-settings-fields rw-auth-form" autoComplete="off" onSubmit={submit}>
    {!attempt ? <>
      {managed && <div className="rw-select-field">{text("Region", "地区")}
        <FieldSelect label={text("Region", "地区")} value={directory} disabled={busy} required
          onChange={(selectedValue) => setDirectory(selectedValue)} options={[
            { value: "", label: text("Choose a region", "选择地区"), disabled: true },
            ...regions.map((base, index) => ({ value: base, label: index === 0 ? text("China", "中国") : text("Global", "全球") })),
            ...(directory && !regions.includes(directory) ? [{ value: directory, label: directory }] : []),
          ]} />
      </div>}
      {loginMethods.includes("browser") && loginMethods.includes("device") && <div className="rw-select-field">
        {text("Sign-in method", "登录方式")}
        <FieldSelect label={text("Sign-in method", "登录方式")} value={mode} disabled={busy}
          onChange={(selectedValue) => setMode(selectedValue === "device" ? "device" : "browser")} options={[
            { value: "browser", label: text("Browser", "浏览器") },
            { value: "device", label: text("Device code", "设备验证码") },
          ]} />
      </div>}
      {enterprise && <label>{text("Enterprise email", "企业邮箱")}
        <input type="email" autoComplete="email" required disabled={busy} value={email} onChange={(event) => setEmail(event.target.value)} />
      </label>}
      <div className="rw-setting-options">
        <Button size="sm" type="submit" isPending={busy} isDisabled={!online || !loginMethods.includes(mode) || !connection.supports(beginMethod)
          || (managed && !directory)}>{text("Sign in", "登录")}</Button>
        {managed && <Button size="sm" type="button" variant="secondary" aria-pressed={enterprise} isDisabled={busy}
          onPress={() => { setEnterprise(!enterprise); if (enterprise) setEmail("") }}>{text("Enterprise sign-in", "企业登录")}</Button>}
      </div>
    </> : <>
      <p role="status">{text("Waiting for authorization…", "等待授权…")}</p>
      {attempt.userCode && <code>{attempt.userCode}</code>}
      <p>{text("Expires", "过期时间")} · {new Date(attempt.expiresAt).toLocaleTimeString()}</p>
      {url && <div className="rw-setting-options">
        <a className="rw-settings-link" href={url} target="_blank" rel="noopener noreferrer">{text("Open authorization page", "打开授权页面")}</a>
        <Button size="sm" type="button" variant="secondary" onPress={async () => {
          try { await copyText(url); setCopyStatus(text("Copied link.", "已复制链接。")) }
          catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
        }}>{text("Copy link", "复制链接")}</Button>
      </div>}
      {copyStatus && <p role="status">{copyStatus}</p>}
      {attempt.mode === "browser" && <label>{text("Full callback URL", "完整回调 URL")}
        <input type="password" autoComplete="new-password" spellCheck={false} value={callback} disabled={busy} onChange={(event) => setCallback(event.target.value)} />
      </label>}
      <div className="rw-setting-options">
        {attempt.mode === "browser" && <Button size="sm" type="submit" isDisabled={!online || busy || !callback.trim() || !connection.supports(completeMethod)}>
          {text("Complete sign-in", "完成登录")}
        </Button>}
        <Button size="sm" type="button" variant="secondary" isDisabled={busy || !online} onPress={() => void cancel()}>{text("Cancel sign-in", "取消登录")}</Button>
      </div>
    </>}
    {error && <p className="rw-error" role="alert">{error}</p>}
    {attempt && error && <Button size="sm" type="button" variant="secondary" isDisabled={!online || busy} onPress={() => {
      setError("")
      setPollKey((key) => key + 1)
    }}>{text("Check authorization status", "检查授权状态")}</Button>}
  </form>
}
