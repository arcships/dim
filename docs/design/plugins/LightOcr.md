# light-ocr

Requirements: [plugin management](../plugin-management.md). Issues: [#1158](https://github.com/arcships/dim/issues/1158). Scope: [ADR 002](../../adr/002-plugin-scope.md) (process: one service per host). Language: TypeScript (the row) and CommonJS (the helper). Milestone: P3, with [Modality](Modality.md).

light-ocr is a first-party plugin that supplies Modality's `local/pp-ocr` OCR target ([Modality §A.2.1](Modality.md#a21-ocr-targets)). Its one process row provides the process-wide service `ocrHelper`, whose `describe` method returns the target's description: the Node running the row, the helper script, and the paths the helper must read. Modality reads that description per call and starts the helper out of process, confined, for each OCR task. The engine is the published npm packages of `@arcships/light-ocr`, pinned exactly `0.5.9` by the plugin's `package-lock.json`; Dim builds and hosts no OCR bundle and RuntimeManager holds no OCR component. The plugin ships with Dim as an **optional** plugin ([PluginManager §2.3](PluginManager.md#23-the-default-configuration-file)): it is not installed and not running until the user installs it, and its dependencies are installed into the user's copy at that moment from its own lock ([PluginManager §3.2](PluginManager.md#32-install), [RuntimeManager §5](RuntimeManager.md#5-interfaces)).

## 0. Implementation boundary

| Part | M1 | Later |
| --- | --- | --- |
| The plugin directory `runtime/plugins/light-ocr/` in this repository, listed under `optional` in `runtime/default-plugins.json` (§2) | yes | |
| The row's `ocrHelper` service and `describe` (§3) | yes | |
| The helper: argv, engine, images, PDFs, artifacts, error mapping (§4, Appendix A) | yes | |
| CPU provider on every platform (§5) | yes | Core ML once a confined run measures it |
| Tests against a fixture engine, no download (Appendix C) | yes | |
| The one real confined run (Appendix C) | acceptance, run by hand | |

In the design's three phases, Phase 2 owns the uninstalled optional item in `plugin.list`, the `plugin_not_found` refusal of enable, disable and reload naming it, the `dim-cli` decode test and the clients' Install action; the plugin directory, row and helper are Phase 3.

## 1. Structure

```text
Modality (Rust)                       plugin row (Node)                helper (one process per task)
  ocr.recognize, target local/pp-ocr
  read ocrHelper.describe() ────────▶ ocrHelper
  HostPlatform::spawn_confined ────────────────────────────────────────▶ node helper.cjs --input … --artifacts …
        read: Node root, plugin dir, input                                     │
        write: task work dir; no network; services: none                       │
  parse result.json ◀──────────────────────────────────────────────────  result.json, text.txt, page-NNNN.png
```

- The row is one TypeScript process row of the `light-ocr` plugin, loaded by the Node runtime like any plugin row. It provides `ocrHelper` and does nothing else: it starts no engine, reads no file and holds no state, so the row is cheap to keep loaded and its reload loses nothing.
- `ocrHelper` is a name a foreign part provides and Rust reads, so it is registered in the host's loader catalog ([rutis usage §3.4](../rutis-usage.md#34-the-catalog)), not on a Rust type.
- The helper is a separate CommonJS file in the plugin directory, run by `process.execPath` — the Node that runs the row — and never by the row's own process. One helper process per task; the engine is created at the start and closed before exit.
- Nothing here names the engine in Modality: Modality publishes the artifacts the helper wrote, and the helper's contract is the one Modality already defines for `local/apple-vision` (Modality §A.2).

## 2. The plugin package

`runtime/plugins/light-ocr/` ships with the release through the `optional` map of the default configuration file, which copies the directory into `<home>/plugins/light-ocr/<version>+<suffix>` at install time and installs its dependencies there. The map's value is relative to `runtime/default-plugins.json`, so its entry reads `"light-ocr": "plugins/light-ocr"`.

```text
runtime/plugins/light-ocr/
  dim-plugin.json       name, version, code (entry index.ts, provides ocrHelper), dependencies "npm"
  index.ts              the row: provides ocrHelper; describe()
  helper.cjs            the OCR helper (Appendix A)
  package.json          private package; dependencies below
  package-lock.json     the trust anchor
```

- `dim-plugin.json` declares `"dependencies": "npm"` and `"version"`; both members are PluginManager's ([PluginManager §2.1](PluginManager.md#21-the-plugin-directory-and-its-manifest)). The copy, never the shipped directory, is what runs.
- `package.json` declares two dependencies: `@arcships/light-ocr` at exactly `0.5.9`, and `@arcships/rutis` at the version `runtime/package-lock.json` pins, because a default or installed plugin resolves `@arcships/rutis` from its own `node_modules` ([RuntimeManager §6](RuntimeManager.md#6-release)).
- `package-lock.json` pins every package with npm `integrity`, including the facade's own pins — runtime `0.1.9`, model `light-ocr-model-ppocrv6-small` `0.3.4` and the platform packages — because the facade declares them as its dependencies. The lock, not a version range, decides what runs, as `runtime/package-lock.json` does for the runtime packages.
- A bump of light-ocr is a Dim change: a new lock in this plugin, reviewed like any dependency change. A newer release ships a new `version`: at the next start an installed copy is re-acquired in the background and the record switches only when the new copy is ready, so the old copy keeps serving until then ([PluginManager §3.1](PluginManager.md#31-startup)). The same path repairs a failed re-acquisition through `plugin.update`, and a plugin whose `@arcships/rutis` no longer matches the runtime is replaced the same way.
- The shipped directory carries no `node_modules` and no build output: `scripts/stage-plugins.mjs` copies it without running `npm ci` ([RuntimeManager §6](RuntimeManager.md#6-release)). No package lifecycle script runs at install, because the install uses `--ignore-scripts`.
- The release carries neither Node nor any engine package: Node is acquired only when a client's `plugin.install`, `plugin.enable` or `plugin.update` — or an installed copy's background re-acquisition — turns the plugin on ([PluginManager §2.5](PluginManager.md#25-runtimes), [RuntimeManager §3.2](RuntimeManager.md#32-acquire)), and the engine packages only by `npm ci` into the user's copy ([RuntimeManager §6](RuntimeManager.md#6-release)). A user who never installs the plugin downloads nothing for it.

## 3. The row and `ocrHelper`

Service `ocrHelper`, scope process, no key, provided by the `light-ocr` row's `provides`. One method:

| Method | Kind | Request | Reply |
| --- | --- | --- | --- |
| `describe` | sync | `{}` | `{contract: 1, target: {providerId: "local", modelId: "pp-ocr"}, label, executable, args, read}` or `null` |

- `contract` is the helper-contract version this plugin implements, `1`. Modality offers the target only when it equals the version Modality implements; an older copy that is still running marks the target unavailable with "the PP-OCR plugin needs an update" until its re-acquisition lands (Modality §A.2.1).
- `target` is `{providerId: "local", modelId: "pp-ocr"}`, so Modality lists the target as `local/pp-ocr`.
- `label` is English display text for clients: `PP-OCR (local)`.
- `executable` is `process.execPath`: the Node installation that runs the row. With `source: system` it is the interpreter on `PATH`; either way Modality records nothing and trusts the description at each call.
- `args` is `[<plugin directory>/helper.cjs]`, an absolute path. Modality appends `--input`, `--artifacts` and `--output-mode` and nothing else.
- `read` is the canonical Node installation root and the canonical plugin directory, each derived through `fs.realpathSync` — of the executable and of the plugin directory — so a symlinked `node` (a package manager's `bin` link) grants the directory the binary really lives in. Modality adds the task's input to these paths, so the helper can load its own script and `node_modules`, the Node addon in the platform package, and the input.

The row never answers a request by doing OCR itself and never inspects the input. `describe` returns the same value for the life of the process; Modality may call it on every task.

## 4. The helper

### 4.1 Invocation and artifacts

`node helper.cjs --input <path> --artifacts <dir> --output-mode <text|layout>`, started by Modality through `HostPlatform::spawn_confined` with `SystemServices::None`, no network, the task's work directory as the only write path, a 30-minute timeout and 64 KiB caps on stdout and stderr (Modality §7, §A.2.1, A14). Cancellation and timeout kill the process; the helper installs no signal handler.

The helper follows the helper contract Modality already defines (Modality §A.2):

- On success it writes `result.json`, `text.txt`, and, in `layout` mode only, `page-NNNN.png` per page image, prints `{"ok": true, "pageCount": N}` on one line and exits 0.
- On a typed refusal it prints `{"ok": false, "error": {"code", "message"}}` on one line and exits 2. Any other exit is a failure.
- It writes every temporary file inside the artifact directory and leaves nothing outside it.
- It sizes each artifact before writing it and refuses with `resource_limit_exceeded` if the task's artifacts would cross 1 GiB. Modality charges the same bound as each file is written, so the directory never holds more than it.
- It sniffs the input from its bytes, never from a declared media type, and accepts PNG and JPEG images and PDF documents.

`result.json` is Modality's document shape: `{text, pageCount, pages: [{page, text, width, height, imagePath?, regions: [{text, confidence, polygon}]}]}`. `text` is every page's text joined with a newline between pages; each `page.text` is its lines joined with a newline; `polygon` is four `[x, y]` pairs in page pixels, clockwise from the top-left, which is the encoding `crates/dim-ocr` already defines for `local/apple-vision`.

### 4.2 Images

An image input is read once and recognized with `createEngine({execution: {provider: "cpu"}}).recognizeEncoded(bytes)`. The result is one page: `page` 1, `width` and `height` from `imageWidth` and `imageHeight`, `regions` from `lines` (`text`, `confidence`, `box` as the polygon), and `text` from the lines in reading order. An image input produces no `page-NNNN.png`: it is not rasterized, and in `layout` mode its page's `imagePath` is absent because the input is already the page image. An `OcrError` from `invalid_image`, `unsupported_pixel_format` or `invalid_argument` is a typed refusal `invalid_input`.

### 4.3 PDFs

A PDF is opened with `createDocumentEngine({engineOptions: {execution: {provider: "cpu"}}}).recognizeDocument(bytes, options)` and its pages are iterated in order. Every page is rasterized and recognized; there is no native-text path, so a PDF's embedded text layer is ignored. The options carry Modality's limits: `maxPages: 500`, `maxPagePixels: 25_000_000`, `maxTotalPixels: 12_500_000_000` (500 pages × 25,000,000, so the total bound never refuses a document Modality's limits accept), `maxFileBytes: 104_857_600` and `dpi: 150`. Every value is inside the facade's own option validation (`packages/light-ocr/src/document.cjs` at 0.5.9: `positiveInteger` at line 73 defaults its maximum to `Number.MAX_SAFE_INTEGER`; `normalizeOptions` lines 81–124, with the option fields at 87–103, caps `maxPages` at 10,000 and `dpi` at 36–600). 12,500,000,000 is below 2^53, so the engine accepts it exactly, and the helper enforces no limit of its own beyond passing these options.

In `layout` mode the helper renders each page a second time, through the platform package's `pdfium` export — the renderer the document engine loads for itself — at the same `dpi`, and writes the bytes as `page-NNNN.png`, setting that page's `imagePath`. The second render is the only way to obtain the page image: the document API returns lines and dimensions, not pixels. A render failure there is `local_runtime_failed`. In `text` mode no page image is rendered beyond the recognition itself.

The document engine has no code for an encrypted PDF: the rasterizer reports a password or permission failure when the document loads. The binding light-ocr loads, `pdfium-native` 0.6.1, maps PDFium's `FPDF_ERR_PASSWORD` to `PASSWORD:Password required or incorrect` and `FPDF_ERR_SECURITY` to `SECURITY:Unsupported security scheme` (`src/napi_helpers.h`, `GetPdfiumErrorMessage`, lines 24–55, the two cases at 37–44), and `src/pdfium_addon.cc:57` rejects the load with that message. The helper reports a load rejection whose message starts with `PASSWORD:` or `SECURITY:` as `encrypted_pdf`; any other load or render failure is `invalid_input` with the renderer's message.

### 4.4 Result and error mapping

| light-ocr | Modality helper contract |
| --- | --- |
| `DocumentPage.index` (1-based) | `pages[].page` |
| `DocumentPage.width`, `height` | `pages[].width`, `pages[].height` |
| `DocumentLine.text` / `OcrLine.text` | `pages[].text` line, `regions[].text` |
| `DocumentLine.confidence` / `OcrLine.confidence` | `regions[].confidence`, clamped to [0, 1] |
| `DocumentLine.box` / `OcrLine.box` (four points) | `regions[].polygon`, four `[x, y]` page pixels, clockwise from top-left |
| `invalid_argument`, `invalid_image`, `unsupported_pixel_format`, `unsupported_capability` | `invalid_input` |
| `resource_limit_exceeded`, `queue_full` | `resource_limit_exceeded` |
| `invalid_model_bundle`, `unsupported_model`, `model_integrity_failed`, `runtime_initialization_failed`, `invalid_engine`, `bundle_io_failed`, `unsupported_platform`, `package_load_failed`, `environment_closing`, `internal_error` | `local_runtime_unavailable` |
| `inference_failed`, `postprocess_failed` | `local_runtime_failed` |
| a PDF the renderer refuses for a password or permission | `encrypted_pdf` |
| any other error (a missing file, a broken install) | `local_runtime_failed`, or `invalid_input` when the input itself is the cause |

The helper's own failures follow the same five codes: a missing argument is `invalid_input`, an input over 100 MiB is `resource_limit_exceeded`, and a platform package that cannot be loaded is `local_runtime_unavailable`.

## 5. Providers

Every engine is created with `execution: {provider: "cpu"}`, on every platform. CPU is the one provider light-ocr publishes for every platform it supports; Core ML exists only for `darwin-arm64` and its confined behaviour has not been measured, and WebGPU exists only for Linux x64 and Windows x64. `execution.provider` is fixed in the helper, not configurable by a task, so a request can never select a provider the confinement profile does not cover.

The CPU path was measured in the profile `HostPlatform::spawn_confined` builds on macOS arm64: the read set {canonical Node installation root, canonical plugin directory, input}, the task's work directory as the only write path, no network and `SystemServices::None` are sufficient. A PNG and two PDFs — one with a text layer, one scanned — recognized with identical lines, boxes and confidences to the unconfined run, about 0.8 s per PNG, the work directory empty after every run and no Core ML cache created.

Enabling Core ML later requires two things: a measured confined run of the Core ML path under the confinement profile, and `OnDeviceRecognition` added to that profile's `SystemServices` ([HostPlatform](../../architecture/rust-agent/plugins/HostPlatform.md)); both are prerequisites, not work this design schedules. light-ocr's own rule keeps `darwin-x64` CPU-only.

## 6. Timing and conflicts

- One helper process per task, started when the task starts and killed on cancel, on timeout, on Modality's unload and on process exit. No OCR state survives a task; a restart interrupts a running task as any host task (Modality §A.6).
- Concurrency is Modality's: at most 4 running OCR tasks per category. Each is one process with its own engine, so two helpers share nothing but the CPU.
- The row has no work in flight: a reload, an update, a re-acquisition or an uninstall of the plugin affects only later calls. A task keeps the description it read at its start, and PluginManager deletes no installed copy while the process runs ([PluginManager §7](PluginManager.md#7-security)), so the directory that description names exists until the process exits. If that copy is nevertheless unreadable when the helper starts — for example, a user deleted it by hand — the helper fails at load, the task settles `local_runtime_failed` as an unexpected helper exit, and its work directory is removed.
- Uninstalling the plugin therefore affects later calls, not tasks already admitted: its copy is left in place, and the next start's cleanup deletes it ([PluginManager §3.1](PluginManager.md#31-startup)). That costs disk — for light-ocr, a replaced copy is about 124 MB, held until the next start.

## 7. Security

- **Trust anchor.** The plugin's `package-lock.json` is the only source of packages, installed by RuntimeManager with `npm ci --omit=dev --ignore-scripts --no-audit --no-fund`; npm verifies every package against the lock's `integrity`, and no lifecycle script runs ([RuntimeManager §3.2](RuntimeManager.md#32-acquire), §7). The registry is RuntimeManager's rule; a mirror adds no trust. The shipped plugin directory is never modified.
- **Untrusted input.** The input document is parsed only inside the confined helper. Modality and the daemon never open it, never parse it and never hand its content to any other process. This is the reason OCR is a host task and not an adapter (Modality §A.2).
- **Confinement.** The helper runs under `HostPlatform::spawn_confined` with `SystemServices::None`: the Node installation root, the plugin directory and the input readable; the task's work directory writable; no network. Measured on macOS arm64 with the CPU provider: nothing is written outside the work directory — the directory is empty after each run — and no Core ML cache is created. On a platform that confines nothing — Windows, and Linux without `bubblewrap` — the helper runs unconfined and Modality records `confined: false` on the task (Modality A14); the plugin makes no claim of its own and detects nothing.
- **No download at run time.** The helper loads only the pinned packages already on disk. It has no network and no updater; a missing or corrupt package is `local_runtime_unavailable`.
- **No secrets.** The helper reads no credentials and contacts no service. Its environment is Modality's allowlist, and the description carries no environment additions.
- **No model context.** Nothing the helper returns enters a model's context; a user moves the result into a Session by ordinary means (Modality §5).

## 8. Rule check

Rules: [principles §1 and §4](../../architecture/rust-agent/principles.md), [ADR 002](../../adr/002-plugin-scope.md) D1–D5.

| Rule | How this document complies |
| --- | --- |
| §4.1 Command events for state owned by others | The plugin owns nothing: the row provides a description, the helper writes into the task work directory Modality created, and Modality owns the task record. |
| §4.2 No withdrawal after handover | Not applicable: the helper hands over files and one exit status; Modality settles the task once. |
| §4.3 Intervene before a write only through before callbacks | Not applicable: no callback and no core entry. |
| §4.4 Core mechanisms carry no business meaning | The plugin asks no change of Modality, HostPlatform or RuntimeManager beyond the generic requirements in §9; the target is data, not a mechanism. |
| §4.6 Model context only from handed-over events | The plugin writes nothing model-visible. |
| §4.7 Plugins interact only through public events and services | The row provides `ocrHelper`; it reads no other plugin's state. Modality reaches the helper only through the description, a public service. |
| §4.8 Generic requirements to mechanism owners | §9 lists the three raised elsewhere; none is new to this document. |
| §4.9 Only facts later decisions use enter the Timeline | No Timeline event. |
| §1 Capability and App Server boundary | Not a Capability: it has no Timeline state and registers no tool, method or prompt source. |
| ADR 002 D1 Scope follows the owner | Process: one helper description per host, used by every task. |
| ADR 002 D2 No wiring modules | No wiring: Modality reads the name per call; the loader catalog resolves it. |
| ADR 002 D3 Dependencies follow scope | Modality's read of `ocrHelper` is an optional per-call read; the plugin depends on nothing but its own packages. |
| ADR 002 D4 Structural and business configuration | There is no user configuration: the provider policy is fixed in the helper and the package set is the lock. |
| ADR 002 D5 Language-neutral contracts | `ocrHelper` and the helper's argv, artifacts and JSON line are JSON and process contracts; the row and helper are one implementation of them. |

## 9. Requirements raised to mechanism owners

This plugin adds no requirement of its own; it uses three already raised by Modality and PluginManager.

| Owner | Requirement | Where |
| --- | --- | --- |
| Modality | List a target a plugin supplies, read `ocrHelper` per call, and start the helper from its description | [Modality §9.1](Modality.md#91-per-task-adapter-lookup), §A.2.1 |
| PluginManager | Install an optional plugin, copy it into the home and install its declared npm dependencies before the row is created | [PluginManager §2.3](PluginManager.md#23-the-default-configuration-file), §3.2, §9 R5 |
| RuntimeManager | `RuntimeService::installPackages`, and `install("node")` for a plugin turned on while Node is off | [RuntimeManager §5](RuntimeManager.md#5-interfaces), §9 R1 |

HostPlatform's `spawn_confined` with `Confinement` already exists; this plugin asks nothing new of it.

## Appendix A. The helper contract

- **argv.** `--input PATH`, `--artifacts DIR`, `--output-mode text|layout`. `--input` and `--artifacts` are required; `--output-mode` defaults to `text`. An unknown argument or a missing value is `invalid_input`.
- **Input.** One regular file, at most 100 MiB, PNG, JPEG or PDF by its first bytes. The declared media type is never used.
- **Directories.** The helper creates nothing outside `--artifacts`; Modality creates that directory inside the task's work directory.
- **Artifacts.** `result.json` (always), `text.txt` (always), `page-NNNN.png` (layout mode, one per rendered PDF page, `NNNN` zero-padded to four digits). Every name is one component.
- **Exit.** 0 with `{"ok": true, "pageCount": N}`; 2 with `{"ok": false, "error": {"code", "message"}}`; any other code is a failure, and Modality reports `local_runtime_failed`.
- **Limits.** 500 pages, 25,000,000 pixels per page, 100 MiB input, 1 GiB of artifacts per task.
- **No state.** One process, one task, no cache and no lock file. The engine is closed before exit.

## Appendix B. Package layout and lock

```json
{
  "name": "light-ocr",
  "version": "<plugin version>",
  "description": "Local PP-OCR for Modality through light-ocr",
  "code": { "entry": "index.ts", "provides": { "ocrHelper": { "call": "sync" } } },
  "dependencies": "npm"
}
```

`package.json`:

```json
{
  "name": "dim-light-ocr",
  "private": true,
  "dependencies": {
    "@arcships/light-ocr": "0.5.9",
    "@arcships/rutis": "<the version runtime/package-lock.json pins>"
  }
}
```

The lock pins the facade, its runtime, model and platform packages, and `@arcships/rutis` with `integrity`. The facade's own dependencies are installed from the facade's declaration, so the lock is the one place the whole set is pinned. `helper.cjs` resolves `@arcships/light-ocr` from the plugin's `node_modules` and nothing else.

## Appendix C. Test strategy

No automated test downloads, installs or loads the real engine.

- **Helper unit tests.** A fixture `node_modules/@arcships/light-ocr` in the test tree exports `createEngine`, `createDocumentEngine`, `getVersion`, `hasPdfSupport` and `OcrError`, with results and failures the test controls. The helper is run as a child process over fixture inputs, and the tests assert argv parsing, artifact names and shapes, `text.txt` content, the polygon conversion, the limit checks, every error mapping in §4.4, and that a refusal writes no partial artifact past the bound. Nothing is fetched; the fixture is a directory in the repository.
- **Row and plugin tests.** The row is loaded with the fixture engine and `describe` is asserted against Modality's A.2.1 shape — `target`, `label`, `executable` = `process.execPath`, `args`, `read`. Presence, absence and removal of the service are Modality's acceptance flows, run with a stub provider (Modality Appendix D item 9); the plugin's own tests cover only the description it publishes.
- **Install tests.** With a fixture plugin that declares `dependencies`, PluginManager's acquisition copies it, installs its locked dependencies from a local fixture registry and renames it into place; the shipped directory is unchanged (PluginManager Appendix C item 13).
- **The one real run.** A single end-to-end acceptance, not in CI, on macOS arm64: install the plugin from the release's optional directory with the real lock from the public registry, run `modality.ocr.recognize` on a one-page PNG and on a two-page PDF in both output modes, and assert the text, the page count, `page-NNNN.png` in layout mode, exit 0, `confined: true`, and that the helper wrote nothing outside its work directory. It installs and loads downloaded native code, so it is run by hand and never as part of a normal test suite.
- **Harness.** Any confined test or harness starts the helper with pipes for stdout and stderr, as `spawn_confined` does. Redirecting them to files outside the granted roots makes Node abort while it initializes (measured: exit 250, five times out of five); a harness that must use files puts them inside a write root.

## Appendix D. Acceptance

1. `plugin.list` shows `light-ocr` with `optional: true` before it is installed; `dimcode plugin install --optional light-ocr` copies it to `<home>/plugins/light-ocr/<version>+<suffix>`, installs its locked dependencies there, and turns it on; the shipped directory has no `node_modules` and is unchanged.
2. With the plugin installed and the row running, `modality.capabilities` lists `local/pp-ocr` beside `local/apple-vision`; with the plugin off, Pending or removed, it lists only `local/apple-vision` and nothing else changes.
3. An image task returns text and regions; a layout task returns the same regions and no page image for an image input; a PDF task returns one page per page; a two-page PDF in layout mode returns `page-0001.png` and `page-0002.png`.
4. The limits are enforced: a 501-page PDF and an input over 100 MiB are `resource_limit_exceeded`; an encrypted PDF is `encrypted_pdf`; a corrupt image is `invalid_input`.
5. Cancel and timeout kill the helper; the task settles cancelled or failed, and the work directory is removed.
6. `confined: true` on macOS; `confined: false` on Windows and on Linux without `bubblewrap`.
7. A newer release's copy re-acquires the installed plugin in the background at the next start into a new `<version>+<suffix>` directory: the record and `dir` switch only when the new copy is ready, the old copy keeps serving and stays on disk until the next start's cleanup, and the target keeps working without a second `enable`; a failed re-acquisition keeps the old copy and shows in `plugin.list` until `plugin.update` repairs it.
8. No test in the repository downloads, installs or loads the real engine; the fixture engine and a stub `ocrHelper` provider cover Modality's flows.
9. In the staged release, `plugins/light-ocr/` holds exactly the five shipped files — `dim-plugin.json`, `index.ts`, `helper.cjs`, `package.json`, `package-lock.json` — with no `node_modules`, and the release contains no Node installation.
