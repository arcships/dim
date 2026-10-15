// Curated stand-in for shiki's default entry, aliased in vite.config.ts.
//
// Highlighting runs on the JavaScript regex engine: the Oniguruma engine needs
// WebAssembly compilation, which the web entry's CSP (no 'wasm-unsafe-eval')
// refuses. `shiki/wasm` is aliased to a stub so its blob is never bundled.
//
// The default entry ships all 235 grammars and 65 themes as lazy chunks, which
// the CLI binary pays for in full even though a chat only ever highlights the
// languages a coding agent writes. `shiki/bundle/web` is the wrong trade for
// us — it drops Go, Rust, Swift, C#, diff and TOML — so the bundle is assembled
// here instead. A fence in an unlisted language renders unhighlighted.
import {
  createBundledHighlighter,
  createSingletonShorthands,
  type DynamicImportLanguageRegistration,
  type DynamicImportThemeRegistration,
} from "@shikijs/core"
import { createJavaScriptRegexEngine } from "@shikijs/engine-javascript"

export * from "@shikijs/core"
// Named by @pierre/diffs for its opt-in "shiki-wasm" mode, which is never selected here.
export { createOnigurumaEngine } from "shiki/engine/oniguruma"
export { createJavaScriptRegexEngine } from "@shikijs/engine-javascript"

export const bundledLanguages: Record<string, DynamicImportLanguageRegistration> = {
  angular_html: () => import("@shikijs/langs/angular-html"),
  bash: () => import("@shikijs/langs/shellscript"),
  c: () => import("@shikijs/langs/c"),
  cmake: () => import("@shikijs/langs/cmake"),
  console: () => import("@shikijs/langs/shellscript"),
  cpp: () => import("@shikijs/langs/cpp"),
  csharp: () => import("@shikijs/langs/csharp"),
  css: () => import("@shikijs/langs/css"),
  csv: () => import("@shikijs/langs/csv"),
  diff: () => import("@shikijs/langs/diff"),
  docker: () => import("@shikijs/langs/docker"),
  dockerfile: () => import("@shikijs/langs/docker"),
  elixir: () => import("@shikijs/langs/elixir"),
  erlang: () => import("@shikijs/langs/erlang"),
  go: () => import("@shikijs/langs/go"),
  graphql: () => import("@shikijs/langs/graphql"),
  groovy: () => import("@shikijs/langs/groovy"),
  haskell: () => import("@shikijs/langs/haskell"),
  hcl: () => import("@shikijs/langs/hcl"),
  html: () => import("@shikijs/langs/html"),
  ini: () => import("@shikijs/langs/ini"),
  java: () => import("@shikijs/langs/java"),
  javascript: () => import("@shikijs/langs/javascript"),
  js: () => import("@shikijs/langs/javascript"),
  json: () => import("@shikijs/langs/json"),
  json5: () => import("@shikijs/langs/json5"),
  jsonc: () => import("@shikijs/langs/jsonc"),
  jsx: () => import("@shikijs/langs/jsx"),
  kotlin: () => import("@shikijs/langs/kotlin"),
  latex: () => import("@shikijs/langs/latex"),
  lua: () => import("@shikijs/langs/lua"),
  make: () => import("@shikijs/langs/make"),
  makefile: () => import("@shikijs/langs/make"),
  markdown: () => import("@shikijs/langs/markdown"),
  md: () => import("@shikijs/langs/markdown"),
  nginx: () => import("@shikijs/langs/nginx"),
  objective_c: () => import("@shikijs/langs/objective-c"),
  perl: () => import("@shikijs/langs/perl"),
  php: () => import("@shikijs/langs/php"),
  powershell: () => import("@shikijs/langs/powershell"),
  proto: () => import("@shikijs/langs/proto"),
  protobuf: () => import("@shikijs/langs/proto"),
  ps1: () => import("@shikijs/langs/powershell"),
  py: () => import("@shikijs/langs/python"),
  python: () => import("@shikijs/langs/python"),
  r: () => import("@shikijs/langs/r"),
  regexp: () => import("@shikijs/langs/regexp"),
  ruby: () => import("@shikijs/langs/ruby"),
  rust: () => import("@shikijs/langs/rust"),
  scala: () => import("@shikijs/langs/scala"),
  scss: () => import("@shikijs/langs/scss"),
  sh: () => import("@shikijs/langs/shellscript"),
  shell: () => import("@shikijs/langs/shellscript"),
  shellscript: () => import("@shikijs/langs/shellscript"),
  sql: () => import("@shikijs/langs/sql"),
  svelte: () => import("@shikijs/langs/svelte"),
  swift: () => import("@shikijs/langs/swift"),
  toml: () => import("@shikijs/langs/toml"),
  ts: () => import("@shikijs/langs/typescript"),
  tsx: () => import("@shikijs/langs/tsx"),
  typescript: () => import("@shikijs/langs/typescript"),
  vue: () => import("@shikijs/langs/vue"),
  xml: () => import("@shikijs/langs/xml"),
  yaml: () => import("@shikijs/langs/yaml"),
  yml: () => import("@shikijs/langs/yaml"),
  zig: () => import("@shikijs/langs/zig"),
}

/** markstream renders with these two and nothing else. */
export const bundledThemes: Record<string, DynamicImportThemeRegistration> = {
  "vitesse-dark": () => import("@shikijs/themes/vitesse-dark"),
  "vitesse-light": () => import("@shikijs/themes/vitesse-light"),
}

export const createHighlighter = createBundledHighlighter<string, string>({
  langs: bundledLanguages,
  themes: bundledThemes,
  engine: () => createJavaScriptRegexEngine(),
})

export const {
  codeToHast,
  codeToHtml,
  codeToTokens,
  codeToTokensBase,
  codeToTokensWithThemes,
  getLastGrammarState,
  getSingletonHighlighter,
} = createSingletonShorthands<string, string>(createHighlighter)
