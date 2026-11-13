import "@fontsource-variable/geist"
import "@fontsource-variable/geist-mono"
import "markstream-vue/index.css"
import "./styles/global.css"
import "./styles/settings.css"
import { createApp, watch } from "vue"
import { createPinia } from "pinia"
import { addAPIProvider } from "@iconify/vue"
import ui from "@nuxt/ui/vue-plugin"
import AppRoot from "./app/AppRoot.vue"

// Icons are bundled via @nuxt/ui clientBundle; disable network fetches under strict CSP.
addAPIProvider("", { resources: [] })
import { loadMarkdownRenderer } from "./features/chat/markdown-renderer"
import { i18n, setAppLocale } from "./i18n"
import { router } from "./router"
import { exchangeBootstrapToken } from "./rpc/client"
import { useAppearanceStore } from "./stores/appearance"
import { useConnectionStore } from "./stores/connection"
import { useUiStore } from "./stores/ui"

watch(i18n.global.locale, (locale) => { document.documentElement.lang = locale }, { immediate: true })
const pinia = createPinia()
useUiStore(pinia)

void Promise.all([exchangeBootstrapToken(), setAppLocale(useAppearanceStore(pinia).language || navigator.language)]).then(() => {
  useConnectionStore(pinia).client.start()
  createApp(AppRoot).use(pinia).use(i18n).use(router).use(ui).mount("#app")
  setTimeout(() => void loadMarkdownRenderer(), 0)
})
