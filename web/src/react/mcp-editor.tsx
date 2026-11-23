import { FieldSelect } from "./field-select"
import { Button } from "@heroui/react"
import type { FormEvent } from "react"
import type { Draft, SecretField } from "../features/settings/mcp-form"

type Text = (en: string, zh: string) => string

function SecretActions({ value, onChange, stored, text, label, forbidSet = false }: {
  value: SecretField["action"]; onChange: (value: SecretField["action"]) => void
  stored?: boolean; text: Text; label: string; forbidSet?: boolean
}) {
  return <FieldSelect<SecretField["action"]> label={label} value={value} onChange={onChange} options={[
    ...(stored ? [{ value: "keep" as const, label: text("Keep", "保留") }] : []),
    { value: "set", label: text("Set", "设置"), disabled: forbidSet },
    ...(stored ? [{ value: "remove" as const, label: text("Remove", "移除") }] : []),
  ]} />
}

export function McpEditor({ draft, editing, dirty, workspace, enabled, canTest, canSave, text, update, save, test, cancel, move }: {
  draft: Draft; editing: boolean; dirty: boolean; workspace: string; enabled: boolean; canTest: boolean; canSave: boolean; text: Text
  update: (patch: Partial<Draft>) => void; save: (event: FormEvent<HTMLFormElement>) => void; test: () => void; cancel: () => void; move?: () => void
}) {
  const oauth = editing ? draft.oauthAction === "set" || (draft.oauthAction === "keep" && draft.oauth) : draft.oauth
  const showOAuth = !draft.secretsInUserScope || !draft.authConfigured
  const editSecret = (index: number, patch: Partial<SecretField>) =>
    update({ secrets: draft.secrets.map((field, position) => position === index ? { ...field, ...patch } : field) })
  return <form className="rw-settings-card rw-settings-fields" autoComplete="off" onSubmit={save}>
    <h2>{editing ? text("Edit MCP server", "编辑 MCP 服务器") : text("Add MCP server", "添加 MCP 服务器")}</h2>
    <fieldset className="rw-mcp-fields" disabled={!enabled}>
      <label>{text("Server ID", "服务器 ID")}<input required value={draft.id} readOnly={editing} onChange={(event) => update({ id: event.target.value })} /></label>
      <div className="rw-select-field">{text("Source", "来源")}
        <FieldSelect label={text("Source", "来源")} value={draft.source} disabled={editing}
          onChange={(selectedValue) => update({ source: selectedValue === "project" ? "project" : "user" })} options={[
            { value: "user", label: text("User", "用户") },
            ...((workspace || draft.source === "project") ? [{ value: "project", label: text("Project", "项目") }] : []),
          ]} />
      </div>
      <div className="rw-select-field">{text("Transport", "传输方式")}
        <FieldSelect label={text("Transport", "传输方式")} value={draft.type} disabled={editing}
          onChange={(selectedValue) => {
            const type = selectedValue
            if (type === "stdio" || type === "http" || type === "sse") update({ type, secrets: [] })
          }} options={[
            { value: "stdio", label: "stdio" },
            { value: "http", label: "HTTP" },
            { value: "sse", label: "SSE" },
          ]} />
      </div>
      <label>{text("Tool timeout (optional, milliseconds)", "工具超时（可选，毫秒）")}
        <input type="number" min="1000" max="86400000" value={draft.timeout} onChange={(event) => update({ timeout: event.target.value })} />
      </label>
      {draft.secretsInUserScope && <div className="rw-settings-card">
        <p>{text("Withheld values are edited in user scope.", "隐藏值需在用户范围内编辑。")}</p>
        {move && <Button type="button" size="sm" variant="secondary" onPress={move}>{text("Move to user to edit", "移到用户范围编辑")}</Button>}
      </div>}
      {draft.type === "stdio" ? <>
        <label>{text("Executable", "可执行程序")}<input required value={draft.command} onChange={(event) => update({ command: event.target.value })} /></label>
        <label>{text("Working directory (optional)", "工作目录（可选）")}<input value={draft.cwd} onChange={(event) => update({ cwd: event.target.value })} /></label>
        {editing && !draft.secretsInUserScope && <div className="rw-select-field">
          {text("Stored arguments (hidden)", "已存参数（隐藏）")} · {draft.storedArgsCount}
          <FieldSelect label={text("Stored arguments (hidden)", "已存参数（隐藏）")} value={draft.argsAction}
            onChange={(selectedValue) => update({ argsAction: selectedValue === "replace" ? "replace" : "keep" })} options={[
              { value: "keep", label: text("Keep stored arguments", "保留已存参数") },
              { value: "replace", label: text("Replace all arguments", "替换全部参数") },
            ]} />
        </div>}
        {!draft.secretsInUserScope && (!editing || draft.argsAction === "replace") && <>
          {draft.args.map((argument, index) => <div className="rw-field-row" key={index}>
            <label>{text("Argument", "参数")} {index + 1}
              <input type="password" autoComplete="new-password" value={argument} onChange={(event) => {
                update({ args: draft.args.map((value, position) => position === index ? event.target.value : value) })
              }} />
            </label>
            <Button size="sm" type="button" variant="tertiary" onPress={() => update({ args: draft.args.filter((_, position) => position !== index) })}>{text("Remove", "移除")}</Button>
          </div>)}
          <Button size="sm" type="button" variant="secondary" onPress={() => update({ args: [...draft.args, ""] })}>{text("Add argument", "添加参数")}</Button>
        </>}
      </> : <>
        {editing && <p>{text("Current endpoint", "当前端点")} · <code>{draft.endpoint}</code></p>}
        {!draft.secretsInUserScope && <label>{editing ? text("Replace URL (blank keeps stored URL)", "替换 URL（留空保留原值）") : "URL"}
          <input inputMode="url" required={!editing} value={draft.url} onChange={(event) => update({ url: event.target.value })} />
        </label>}
        {!editing && <label className="rw-recording-toggle">
          <input type="checkbox" disabled={!!draft.token} checked={draft.oauth} onChange={(event) => update({ oauth: event.target.checked })} />
          {text("Configure OAuth", "配置 OAuth")}
        </label>}
        {editing && showOAuth && <label>{text("OAuth configuration", "OAuth 配置")}
          <SecretActions stored value={draft.oauthAction} onChange={(oauthAction) => update({ oauthAction })} text={text} label={text("OAuth configuration", "OAuth 配置")} />
        </label>}
        {((!editing && oauth) || (editing && draft.oauthAction === "set" && showOAuth)) && <>
          <p>{text("OAuth cannot be combined with a bearer token or Authorization header.", "OAuth 不能与 Bearer 令牌或 Authorization 请求头一起使用。")}</p>
          <label>{text("Client ID (optional)", "客户端 ID（可选）")}<input value={draft.clientId} onChange={(event) => update({ clientId: event.target.value })} /></label>
          <label>{text("Callback port (default 1456)", "回调端口（默认 1456）")}
            <input type="number" min="1" max="65535" value={draft.callbackPort} onChange={(event) => update({ callbackPort: event.target.value })} />
          </label>
          <label>{text("Scopes (space separated)", "权限范围（空格分隔）")}<input value={draft.scopes} onChange={(event) => update({ scopes: event.target.value })} /></label>
          {editing && !draft.secretsInUserScope && <label>{text("Metadata URL action", "元数据 URL 操作")}
            <SecretActions stored value={draft.metadataAction} onChange={(metadataAction) => update({ metadataAction })} text={text} label={text("Metadata URL action", "元数据 URL 操作")} />
          </label>}
          {!draft.secretsInUserScope && (!editing || draft.metadataAction === "set") && <label>
            {text("Authorization metadata URL", "授权元数据 URL")}
            <input type="url" required={editing} value={draft.metadataUrl} onChange={(event) => update({ metadataUrl: event.target.value })} />
          </label>}
        </>}
        {!draft.secretsInUserScope && editing && <label>{text("Bearer token action", "Bearer 令牌操作")}
          <SecretActions stored value={draft.bearerAction} forbidSet={oauth} onChange={(bearerAction) => update({ bearerAction, token: "" })}
            text={text} label={text("Bearer token action", "Bearer 令牌操作")} />
        </label>}
        {!draft.secretsInUserScope && (!editing ? !oauth : draft.bearerAction === "set") && <label>
          {text("Bearer token (write-only)", "Bearer 令牌（只写）")}
          <input type="password" required={editing} autoComplete="new-password" value={draft.token} onChange={(event) => update({ token: event.target.value })} />
        </label>}
      </>}
      {!draft.secretsInUserScope && <>
        <h3>{draft.type === "stdio" ? text("Environment variables", "环境变量") : text("HTTP headers", "HTTP 请求头")}</h3>
        <p>{text("Stored values are withheld. Set replaces a value and Remove deletes it.", "已存值保持隐藏。设置会替换原值，移除会删除原值。")}</p>
        {draft.secrets.map((field, index) => <div className="rw-field-row rw-credential-row" key={index}>
          <label>{text("Field name", "字段名称")}
            <input required readOnly={field.stored} value={field.name} onChange={(event) => editSecret(index, { name: event.target.value })} />
          </label>
          <SecretActions stored={field.stored} value={field.action} onChange={(action) => editSecret(index, { action, value: "" })}
            text={text} label={`${text("Field action", "字段操作")} ${index + 1}`} />
          {field.action === "set" && <label>{text("New value", "新值")}
            <input type="password" autoComplete="new-password" value={field.value} onChange={(event) => editSecret(index, { value: event.target.value })} />
          </label>}
          <Button size="sm" type="button" variant="tertiary" onPress={() => update({ secrets: draft.secrets.filter((_, position) => position !== index) })}>
            {text("Cancel field change", "取消字段更改")}
          </Button>
        </div>)}
        <Button size="sm" type="button" variant="secondary" onPress={() => update({ secrets: [...draft.secrets, { name: "", action: "set", value: "" }] })}>{text("Add field", "添加字段")}</Button>
      </>}
      {!editing && draft.source === "user" && <label className="rw-recording-toggle">
        <input type="checkbox" checked={draft.enabled} onChange={(event) => update({ enabled: event.target.checked })} />{text("Enable after saving", "保存后启用")}
      </label>}
      {draft.source === "project" && <p>{text("Project servers must be saved and explicitly enabled locally before testing.", "项目服务器须先保存并在本地明确启用，之后才可测试。")}</p>}
      <div className="rw-setting-options">
        <Button size="sm" type="submit" isDisabled={!canSave || !dirty}>{text("Save server", "保存服务器")}</Button>
        <Button size="sm" type="button" variant="secondary" isDisabled={!canTest || (draft.source === "project" && (!editing || !draft.enabled))}
          onPress={test}>{text("Test draft connection", "测试草稿连接")}</Button>
        <Button size="sm" type="button" variant="secondary" onPress={cancel}>{text("Cancel", "取消")}</Button>
      </div>
    </fieldset>
  </form>
}
