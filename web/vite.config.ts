import { defineConfig, normalizePath } from "vite"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"
import { existsSync, readFileSync, readdirSync } from "node:fs"
import { fileURLToPath } from "node:url"

/** The `dimcode web` entry the dev server forwards /api and /ws to. */
const target = process.env.DIM_WEB_TARGET ?? "http://127.0.0.1:43201"
const unused = fileURLToPath(new URL("./src/vendor/unused-optional.ts", import.meta.url))

export default defineConfig({
  base: "/",
  resolve: {
    alias: [
      { find: "#workspace-store", replacement: fileURLToPath(new URL("./src/react/define-store.ts", import.meta.url)) },
      { find: "#workspace-i18n", replacement: fileURLToPath(new URL("./src/react/i18n.ts", import.meta.url)) },
      // Curated shiki bundle on the JavaScript regex engine (see src/vendor/shiki-web.ts).
      { find: /^shiki$/, replacement: fileURLToPath(new URL("./src/vendor/shiki-web.ts", import.meta.url)) },
      { find: /^shiki\/wasm$/, replacement: unused },
      { find: /^client-only$/, replacement: unused },
    ],
  },
  plugins: [react(), tailwindcss(), {
    name: "third-party-notices",
    apply: "build",
    generateBundle(_options, bundle) {
      for (const asset of Object.values(bundle)) {
        if (asset.type !== "chunk") continue
        const vueRuntime = Object.keys(asset.modules).find((module) =>
          /\/node_modules\/(?:vue|vue-i18n|pinia|@vue\/runtime-(?:core|dom))\//.test(normalizePath(module)) || /\.vue(?:\?|$)/.test(module))
        if (vueRuntime) this.error(`The React Web entry imports a Vue runtime module: ${vueRuntime}`)
      }
      const lock = JSON.parse(readFileSync(new URL("./package-lock.json", import.meta.url), "utf8"))
      const webRoot = normalizePath(fileURLToPath(new URL("./", import.meta.url)))
      const bundledDirs = new Set<string>()
      for (const rawId of this.getModuleIds()) {
        const id = normalizePath(rawId.split("?", 1)[0]!)
        const rel = id.startsWith(webRoot) ? id.slice(webRoot.length) : id
        if (!rel.startsWith("node_modules/")) continue
        const parts = rel.split("/")
        let match = ""
        for (let i = 0; i < parts.length; i++) {
          if (parts[i] !== "node_modules") continue
          const scope = parts[i + 1]
          if (!scope) break
          const len = scope.startsWith("@") ? 3 : 2
          if (i + len <= parts.length) match = parts.slice(0, i + len).join("/")
        }
        if (match && lock.packages[match]) bundledDirs.add(match)
      }
      const notices = [readFileSync(new URL("../NOTICE", import.meta.url), "utf8")]
      for (const directory of Object.keys(lock.packages).sort()) {
        const pkg = lock.packages[directory]
        if (!directory || !bundledDirs.has(directory)) continue
        const root = new URL(`./${directory}/`, import.meta.url)
        if (pkg.optional && !existsSync(root)) continue
        const name = directory.slice(directory.lastIndexOf("node_modules/") + "node_modules/".length)
        const files = readdirSync(root, { withFileTypes: true })
          .filter(file => file.isFile() && /^(licen[cs]e|copying|notice|ofl)(?:[.-]|$)/i.test(file.name))
          .map(file => new URL(file.name, root))
        if (files.length === 0) files.push(new URL(`./licenses/${name}/${pkg.version}/LICENSE`, import.meta.url))
        notices.push(`${name}@${pkg.version}`, ...files.map(file => readFileSync(file, "utf8")))
      }
      this.emitFile({ type: "asset", fileName: "THIRD-PARTY-NOTICES.txt", source: notices.join("\n\n") })
    },
  }],
  server: {
    proxy: {
      "/api": { target, changeOrigin: true, headers: { origin: target } },
      "/ws": { target, changeOrigin: true, ws: true, headers: { origin: target } },
    },
  },
  build: {
    assetsInlineLimit: 0,
  },
})
