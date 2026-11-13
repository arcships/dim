import { defineConfig } from "vite"
import vue from "@vitejs/plugin-vue"
import ui from "@nuxt/ui/vite"
import { existsSync, readFileSync, readdirSync } from "node:fs"

/** The `dimcode web` entry the dev server forwards /api and /ws to. */
const target = process.env.DIM_WEB_TARGET ?? "http://127.0.0.1:43201"
const unused = new URL("./src/vendor/unused-optional.ts", import.meta.url).pathname

export default defineConfig({
  base: "/",
  resolve: {
    alias: [
      // Curated shiki bundle on the JavaScript regex engine (see src/vendor/shiki-web.ts).
      { find: /^shiki$/, replacement: new URL("./src/vendor/shiki-web.ts", import.meta.url).pathname },
      { find: /^shiki\/wasm$/, replacement: unused },
      { find: /^@terrastruct\/d2$/, replacement: unused },
    ],
  },
  plugins: [vue(), ui({
    colorMode: false,
    autoImport: { dts: "src/auto-imports.d.ts" },
    components: { dts: "src/components.d.ts" },
    ui: {
      colors: {
        primary: "green",
        neutral: "neutral",
      },
    },
    icon: {
      mode: "svg",
      clientBundle: {
        scan: true,
        icons: [],
      },
    },
  }), {
    name: "third-party-notices",
    apply: "build",
    generateBundle() {
      const lock = JSON.parse(readFileSync(new URL("./package-lock.json", import.meta.url), "utf8"))
      const webRoot = new URL("./", import.meta.url).pathname
      const bundledDirs = new Set<string>()
      for (const rawId of this.getModuleIds()) {
        if (rawId === "virtual:nuxt-ui-icons") bundledDirs.add("node_modules/@iconify-json/lucide")
        const id = rawId.split("?", 1)[0]!.replaceAll("\\", "/")
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
    outDir: "dist",
    assetsInlineLimit: 0,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes("/node_modules/")) return undefined
          if (/\/node_modules\/(@vue|vue|vue-router|vue-i18n|@intlify|pinia)\//.test(id)) return "vendor-vue"
          return undefined
        },
      },
    },
  },
})
