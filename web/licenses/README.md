# Web dependency notices

These exact-version records cover production packages whose npm archives omit a license file. The Web build reads an installed package's own license and notice files first, then uses the matching version below. An unrecorded omission fails the build.

| Package | Source |
| --- | --- |
| `@vue/devtools-api` 6.6.4 | [Upstream MIT license at the package's gitHead](https://github.com/vuejs/vue-devtools/blob/df6ab6bb7791a7a525a97990de73b3ea5e9a1941/LICENSE) |
| `lru_map` 0.4.1 | Full MIT terms in the published README, also present [at gitHead](https://github.com/rsms/js-lru/blob/77c51e90dbcf4ffc0d27e6bc04a012a6b54a05e1/README.md#mit-license) |
| `markstream-core` 2.0.19 | [Monorepo license at its release tag's commit](https://github.com/Simon-He95/markstream-vue/blob/3e4b2f2033dfea15aa36df1b3c4741f0ec3de2b4/license) |
| `stream-markdown-parser` 1.2.20 | [The same monorepo license at its release tag's commit](https://github.com/Simon-He95/markstream-vue/blob/3e4b2f2033dfea15aa36df1b3c4741f0ec3de2b4/license) |
| `stream-diffs` 0.0.2 | Published package metadata and [tagged source](https://github.com/Simon-He95/stream-diffs/tree/8047a31b224b12227bb0656326ada020a6a5f72d) declare MIT and author Simon He but provide no copyright notice or license text. Its record preserves those facts and the standard MIT terms without inferring a holder or year. |
| `@iconify-json/lucide` 1.2.154 | `info.json` and `package.json` declare ISC from Lucide Contributors; [upstream Lucide license](https://github.com/lucide-icons/lucide/blob/main/LICENSE) matches the notice shipped in `lucide-vue-next`. |
| `vaul-vue` 0.4.1 | [Upstream MIT license](https://github.com/unovue/vaul-vue/blob/main/LICENSE) from the repository referenced by `package.json`. |

Check these records when updating their packages. Geist and Geist Mono ship their own OFL files in npm and need no override here.
