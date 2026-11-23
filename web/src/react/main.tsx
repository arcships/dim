import "@fontsource-variable/geist"
import "@fontsource-variable/geist-mono"
import "./workspace.css"
import { createRoot } from "react-dom/client"
import { exchangeBootstrapToken } from "../rpc/client"
import { setAppLocale } from "./i18n"
import { WorkspaceApp } from "./workspace"
import { appearance, connection } from "./stores"

const root = document.getElementById("app")
if (root === null) throw new Error("Missing workspace root")

void Promise.all([exchangeBootstrapToken(), setAppLocale(appearance.language || navigator.language)]).then(() => {
  connection.client.start()
  createRoot(root).render(<WorkspaceApp />)
})
