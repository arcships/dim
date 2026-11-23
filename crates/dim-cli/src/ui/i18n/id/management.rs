pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Loading..." => "Memuat…",
        "No entries." => "Tidak ada entri.",
        "No matching entry." => "Tidak ada entri yang cocok.",
        "Issues" => "Masalah",
        "sources" => "Sumber",
        "skills" => "Skill",
        "hooks" => "hook",
        "scheduled jobs" => "tugas terjadwal",
        "models" => "model",
        "workflow definitions" => "definisi alur kerja",
        "command rules" => "Aturan perintah",
        "enabled" => "aktif",
        "shadowed by {name}" => "tertutup oleh {name}",
        "Reloaded." => "Dimuat ulang.",
        "Enabled {name}." => "{name} diaktifkan.",
        "Disabled {name}." => "{name} dinonaktifkan.",
        "Reloaded {area}: {entries} entries, {disabled} disabled." => {
            "{area} dimuat ulang: {entries} entri, {disabled} nonaktif."
        }
        "Reloaded {area}: {entries} entries." => "{area} dimuat ulang: {entries} entri.",
        "Hooks" => "Hooks",
        "invalid" => "tidak valid",
        "Saved." => "Disimpan.",
        "Saved {name}." => "{name} disimpan.",
        "No change." => "Tidak ada perubahan.",
        "Your text is kept in {path}." => "Teks Anda tersimpan di {path}.",
        "The user's own document; always in effect." => "Dokumen milik pengguna; selalu berlaku.",
        "There is no hooks file there." => "Tidak ada file hook di sana.",
        "Owned by plugin {plugin}." => "Milik plugin {plugin}.",
        "The user document turns every hook off (disableAllHooks)." => {
            "Dokumen pengguna mematikan semua hook (disableAllHooks)."
        }
        "Enter toggle" => "Enter alih",
        "Ctrl+E edit" => "Ctrl+E edit",
        "Ctrl+N add" => "Ctrl+N tambah",
        "Ctrl+R reload" => "Ctrl+R muat ulang",
        "Ctrl+T run" => "Ctrl+T jalankan",
        "Ctrl+X remove" => "Ctrl+X hapus",
        "Tab inspect" => "Tab periksa",
        "Remove {name}?" => "Hapus {name}?",
        "↑↓ scroll · Esc back" => "↑↓ gulir · Esc kembali",
        "Added." => "Ditambahkan.",
        "Removed {name}." => "{name} dihapus.",
        "unavailable" => "tidak tersedia",
        "Reset {name}?" => "Reset {name}?",
        "Enter confirm" => "Enter konfirmasi",
        "Esc cancel" => "Esc batal",
        "Reset {name} to its catalog entry." => "{name} direset ke entri katalog.",
        "Ran {name}." => "{name} dijalankan.",
        "The user document changed since it was read; nothing was written." => {
            "Dokumen pengguna berubah sejak dibaca; tidak ada yang ditulis."
        }
        "The job changed since it was read; nothing was written." => {
            "Tugas berubah sejak dibaca; tidak ada yang ditulis."
        }
        "The jobs changed since they were read; nothing was written." => {
            "Tugas berubah sejak dibaca; tidak ada yang ditulis."
        }
        "Your next edit opens the current document." => "Edit berikutnya membuka dokumen saat ini.",
        "Your next add reopens your text." => "Tambah berikutnya membuka kembali teks Anda.",
        "Scheduler" => "Penjadwal",
        "jobs" => "tugas",
        "Added {name}." => "{name} ditambahkan.",
        "Run {name}: {outcome}." => "{name} dijalankan: {outcome}.",
        "Recent runs of {name}" => "Eksekusi terbaru {name}",
        "No run yet." => "Belum ada eksekusi.",
        "No next run." => "Tidak ada eksekusi berikutnya.",
        "Next run {instant}." => "Eksekusi berikutnya {instant}.",
        "Last run {outcome} at {instant}." => "Eksekusi terakhir {outcome} pada {instant}.",
        "every {minutes} minutes" => "setiap {minutes} menit",
        "at {instant}" => "pada {instant}",
        "cron {expression}" => "cron {expression}",
        "cron {expression} in {zone}" => "cron {expression} di {zone}",
        "The job changed since it was read; nothing was removed." => {
            "Tugas berubah sejak dibaca; tidak ada yang dihapus."
        }
        "The Scheduler has no area document." => "Penjadwal tidak memiliki dokumen area.",
        "No scheduled job has that id." => "Tidak ada tugas terjadwal dengan id itu.",
        "A scheduled job's run takes no input document." => {
            "Eksekusi job terjadwal tidak menerima dokumen masukan."
        }
        "An MCP server's test takes no input document." => {
            "Uji server MCP tidak menerima dokumen masukan."
        }
        "This daemon cannot manage {area}." => "Tidak dapat mengelola {area}.",
        "This daemon lists {area} but cannot toggle them." => {
            "Daemon ini menampilkan {area}, tetapi tidak dapat mengalihkannya."
        }
        "This daemon lists {area} but cannot edit them." => {
            "Daemon ini menampilkan {area}, tetapi tidak dapat mengeditnya."
        }
        "This daemon lists {area} but cannot reload them." => {
            "Daemon ini menampilkan {area}, tetapi tidak dapat memuat ulang daftarnya."
        }
        "This daemon lists {area} but cannot add them." => {
            "Daemon ini menampilkan {area}, tetapi tidak dapat menambahnya."
        }
        "This daemon lists {area} but cannot remove them." => {
            "Daemon ini menampilkan {area}, tetapi tidak dapat menghapusnya."
        }
        "This daemon lists {area} but cannot run them." => {
            "Daemon ini menampilkan {area}, tetapi tidak dapat menjalankannya."
        }
        "This daemon lists {area} but cannot inspect them." => {
            "Daemon ini menampilkan {area}, tetapi tidak dapat melihat detail atau riwayatnya."
        }
        "The document deleted {field}, which the owner cannot clear; nothing was written." => {
            "Dokumen menghapus {field}, yang tidak dapat dibersihkan pemilik; tidak ada yang ditulis."
        }
        "The document changes no field; nothing was written." => {
            "Dokumen tidak mengubah bidang apa pun; tidak ada yang ditulis."
        }
        "This workspace's hook settings changed since they were read; nothing was written." => {
            "Pengaturan hook ruang kerja ini berubah sejak dibaca; tidak ada yang ditulis."
        }
        "Models" => "Model",
        "catalog" => "katalog",
        "overrides" => "penimpaan",
        "added" => "ditambahkan",
        "Models · {name}" => "Model · {name}",
        "undeclared" => "belum dideklarasikan",
        "Context window {window} · max output {output}" => {
            "Jendela konteks {window} · output maks {output}"
        }
        "Endpoint {endpoint}" => "Endpoint {endpoint}",
        "Endpoint {endpoint} · override {value}" => "Endpoint {endpoint} · penimpaan {value}",
        "The provider reports it unavailable." => "Penyedia melaporkannya tidak tersedia.",
        "Limits undeclared; edit the model to declare them." => {
            "Batas belum dideklarasikan; edit modelnya untuk mendeklarasikan."
        }
        "Sign in to the account to use its models." => "Masuk ke akun untuk memakai modelnya.",
        "The signed-in account changed; reload to list its models." => {
            "Akun yang masuk berubah; muat ulang untuk menampilkan modelnya."
        }
        "The provider has no catalog yet." => "Penyedia ini belum punya katalog.",
        "The provider listed no models." => "Penyedia tidak menampilkan model apa pun.",
        "Listed {count} models." => "Menampilkan {count} model.",
        "Refreshed the models." => "Model diperbarui.",
        "Could not list models: {error}. Showing the stored list." => {
            "Tidak dapat menampilkan model: {error}. Menampilkan daftar tersimpan."
        }
        "The signed-in account changed; the refresh cleared preferences, overrides and disables." => {
            "Akun yang masuk berubah; muat ulang menghapus preferensi, penimpaan, dan penonaktifan."
        }
        "The models changed since they were read; nothing was removed." => {
            "Model berubah sejak dibaca; tidak ada yang dihapus."
        }
        "The models changed since they were read; nothing was written." => {
            "Model berubah sejak dibaca; tidak ada yang ditulis."
        }
        "No provider has that id." => "Tidak ada penyedia dengan id itu.",
        "{name} is unavailable ({reason}); its enablement is kept as it is." => {
            "{name} tidak tersedia ({reason}); status aktifnya dipertahankan."
        }
        "No model has that id." => "Tidak ada model dengan id itu.",
        "This model cannot be removed." => "Model ini tidak dapat dihapus.",
        "A Provider's models have no area document." => "Model penyedia tidak punya dokumen area.",
        "Models have no run operation." => "Model tidak punya operasi jalankan.",
        "The document is not an object; nothing was written." => {
            "Dokumen bukan objek; tidak ada yang ditulis."
        }
        "The document changes no editable field; nothing was written." => {
            "Dokumen tidak mengubah bidang yang dapat diedit; tidak ada yang ditulis."
        }
        "The model document names no model." => "Dokumen model tidak menamai model.",
        "The provider has no model catalog to write." => {
            "Penyedia ini tidak punya katalog model untuk ditulis."
        }
        "ID {id}" => "ID {id}",
        "Source {source}" => "Sumber {source}",
        "Status {status}" => "Status {status}",
        "Tools {value}" => "Alat {value}",
        "Vision {value}" => "Visi {value}",
        "Reasoning {value}" => "Penalaran {value}",
        "Modalities {list}" => "Modalitas {list}",
        "Description {text}" => "Deskripsi {text}",
        "Release date {date}" => "Tanggal rilis {date}",
        "Pricing {value}" => "Harga {value}",
        "Rate limit {value}" => "Batas laju {value}",
        "Matched provider {id}" => "Penyedia cocok {id}",
        "Matched model {id}" => "Model cocok {id}",
        "Match strategy {strategy}" => "Strategi cocok {strategy}",
        "Capability overrides {fields}" => "Penimpaan kemampuan {fields}",
        "yes" => "ya",
        "no" => "tidak",
        "not supported" => "tidak didukung",
        "supported" => "didukung",

        "Workflow" => "Alur kerja",
        "Saved {name}. It stays disabled until you enable it." => {
            "{name} disimpan. Tetap nonaktif sampai Anda mengaktifkannya."
        }
        "The definitions changed since they were read; nothing was written." => {
            "Definisi berubah sejak dibaca; tidak ada yang ditulis."
        }
        "A definition's name is its key; add it to save it under another name." => {
            "Nama definisi adalah kuncinya; tambahkan untuk menyimpannya dengan nama lain."
        }
        "No project definition is named {name}." => "Tidak ada definisi proyek bernama {name}.",
        "No workflow definition is named {name}." => {
            "Tidak ada definisi alur kerja bernama {name}."
        }
        "Saved in the user workflow source; always enabled." => {
            "Disimpan di sumber alur kerja pengguna; selalu aktif."
        }
        "Starts only after you enable it." => "Hanya berjalan setelah Anda mengaktifkannya.",
        "Defined by package {package}." => "Didefinisikan oleh paket {package}.",
        "{name} — {description} (required)" => "{name} — {description} (wajib)",
        "{name} — {description}" => "{name} — {description}",
        "{name} — required" => "{name} — wajib",
        "{name} — default: {default}" => "{name} — default: {default}",
        "and {count} more" => "dan {count} lainnya",
        "Recent runs" => "Jalan terbaru",
        "A workflow definition is saved as an object with a source and a document." => {
            "Definisi alur kerja disimpan sebagai objek dengan sumber dan dokumen."
        }
        "A definition is saved to the user source or the project source." => {
            "Definisi disimpan ke sumber pengguna atau sumber proyek."
        }
        "Started {name}: {run_id}." => "{name} dimulai: {run_id}.",
        "Parameters are a JSON object of strings." => "Parameter adalah objek JSON berisi string.",
        "The list reached its size limit; later definitions are not shown." => {
            "Daftar mencapai batas ukurannya; definisi berikutnya tidak ditampilkan."
        }
        "No workflow definition named {name} is in the list, which reached its size limit." => {
            "Tidak ada definisi workflow bernama {name} dalam daftar, yang mencapai batas ukurannya."
        }
        "MCP" => "MCP",
        "MCP servers" => "server MCP",
        "Ctrl+T test" => "Ctrl+T uji",
        "sign-in required" => "perlu masuk",
        "signed in" => "sudah masuk",
        "sign-in expired" => "sesi masuk kedaluwarsa",
        "missing scopes: {scopes}" => "cakupan yang kurang: {scopes}",
        "Executable: {command}" => "Eksekutabel: {command}",
        "Stored arguments (hidden): {count}" => "Argumen tersimpan (disembunyikan): {count}",
        "Working directory: {cwd}" => "Direktori kerja: {cwd}",
        "Environment names: {names}" => "Nama lingkungan: {names}",
        "Endpoint: {endpoint}" => "Endpoint: {endpoint}",
        "Header names: {names}" => "Nama header: {names}",
        "Bearer token: {state}" => "Token Bearer: {state}",
        "OAuth: configured (client ID {id})" => "OAuth: terkonfigurasi (ID klien {id})",
        "OAuth: configured" => "OAuth: terkonfigurasi",
        "OAuth: not configured" => "OAuth: tidak terkonfigurasi",
        "Metadata URL: {state}" => "URL metadata: {state}",
        "configured" => "terkonfigurasi",
        "Owned by a plugin; change it through Plugins." => "Dimiliki plugin; ubah melalui Plugin.",
        "MCP entry {id}" => "Entri MCP {id}",
        "Connection {number} · {status}" => "Koneksi {number} · {status}",
        "Tools have not been discovered." => "Alat belum ditemukan.",
        "No live session connections." => "Tidak ada koneksi sesi aktif.",
        "Tools of {id}" => "Alat {id}",
        "The server exposes no tools." => "Server tidak menyediakan alat.",
        "The add document needs an id." => "Dokumen tambah memerlukan id.",
        "The add document needs a source of user or project." => {
            "Sumber dokumen tambah harus user atau project."
        }
        "The add document needs a configuration object." => {
            "Dokumen tambah memerlukan objek configuration."
        }
        "{id} is already in mcp list; remove it first or choose another id." => {
            "{id} sudah ada di mcp list; hapus dulu atau pilih id lain."
        }
        "{id} is already in mcp list; choose another id, or change it through its plugin." => {
            "{id} sudah ada di mcp list; pilih id lain atau ubah lewat plugin-nya."
        }
        "This entry's settings changed since they were read; nothing was written." => {
            "Pengaturan entri ini berubah sejak dibaca; tidak ada yang ditulis."
        }
        "The MCP entry changed since it was read; nothing was written." => {
            "Entri MCP berubah sejak dibaca; tidak ada yang ditulis."
        }
        "The MCP entries changed since they were read; nothing was written." => {
            "Entri MCP berubah sejak dibaca; tidak ada yang ditulis."
        }
        "The MCP entry changed since it was read; nothing was removed." => {
            "Entri MCP berubah sejak dibaca; tidak ada yang dihapus."
        }
        "MCP's documents belong to one entry; this save names none." => {
            "Dokumen MCP milik satu entri; penyimpanan ini tidak menyebut entri."
        }
        "{id} is in more than one MCP source: {sources}. Name one with --source." => {
            "{id} ada di lebih dari satu sumber MCP: {sources}. Sebutkan salah satunya dengan --source."
        }
        "The action names no MCP source; a row carries the source its owner reported." => {
            "Tindakan ini tidak menyebutkan sumber MCP; setiap baris membawa sumber yang dilaporkan pemiliknya."
        }
        "No MCP server {id} is in source {source}." => {
            "Tidak ada server MCP {id} di sumber {source}."
        }
        "No MCP server has that id." => "Tidak ada server MCP dengan id itu.",
        "This workspace has no project MCP source." => {
            "Workspace ini tidak memiliki sumber MCP proyek."
        }
        "Enabled {id} ({source})." => "{id} ({source}) diaktifkan.",
        "Disabled {id} ({source})." => "{id} ({source}) dinonaktifkan.",
        "It runs this workspace's .mcp.json entry, including later edits to it, in active and new sessions." => {
            "Ini menjalankan entri .mcp.json workspace ini, termasuk perubahan berikutnya, di sesi aktif dan baru."
        }
        "{id} is not in mcp list; withdrew any stored enablement of it in this workspace." => {
            "{id} tidak ada di mcp list; pengaktifan tersimpan untuknya di workspace ini ditarik."
        }
        "The MCP catalog could not be read ({error}); withdrew this workspace's enablement of {id}. A user entry with this ID is unchanged." => {
            "Katalog MCP tidak dapat dibaca ({error}); pengaktifan {id} di workspace ini ditarik. Entri pengguna dengan ID ini tidak berubah."
        }
        "MCP configuration: {error}" => "Konfigurasi MCP: {error}",
        "The server rejected the stored credential; sign in again with mcp login." => {
            "Server menolak kredensial tersimpan; masuk lagi dengan mcp login."
        }
        "The server requires additional scopes: {scopes}." => {
            "Server memerlukan cakupan tambahan: {scopes}."
        }
        "The server requires authorization; run mcp login ID." => {
            "Server memerlukan otorisasi; jalankan mcp login ID."
        }
        "The probe ended: {status}." => "Probe berakhir: {status}.",

        "Command rules" => "Aturan perintah",
        "user" => "pengguna",
        "Matches only this command." => "Hanya cocok dengan perintah ini.",
        "Matches commands whose first words are these." => {
            "Cocok dengan perintah yang kata awalnya ini."
        }
        "Allows a simple command that would otherwise ask." => {
            "Mengizinkan perintah sederhana yang biasanya ditanyakan."
        }
        "Denies the command under every preset." => "Menolak perintah itu pada setiap preset.",
        "Applies in every Session, in every workspace." => {
            "Berlaku di setiap Sesi, di setiap workspace."
        }
        "Rule added: {rule}." => "Aturan ditambahkan: {rule}.",
        "Rule already present: {rule}." => "Aturan sudah ada: {rule}.",
        "Rule removed: {rule}." => "Aturan dihapus: {rule}.",
        "That command rule is no longer listed; nothing was removed." => {
            "Aturan perintah itu tidak lagi terdaftar; tidak ada yang dihapus."
        }
        "Command rules changed since they were read; nothing was removed." => {
            "Aturan perintah berubah sejak dibaca; tidak ada yang dihapus."
        }
        "A command rule is DECISION MATCH COMMAND, such as deny prefix git push." => {
            "Aturan perintah ditulis DECISION MATCH COMMAND, misalnya deny prefix git push."
        }
        "Command: {command}" => "Perintah: {command}",
        "Use the line prompt or dimcode permissions." => {
            "Gunakan prompt baris atau dimcode permissions."
        }
        "Tool call timeout: {millis} ms" => "Batas waktu panggilan tool: {millis} ms",
        "Variables: {names}" => "Variabel: {names}",
        "Unset variables: {names}" => "Variabel yang belum diset: {names}",
        "The MCP source changed since the preview; no entry was imported, and an entry it would have replaced may now be disabled. Preview again." => {
            "Sumber MCP berubah sejak pratinjau; tidak ada entri yang diimpor, dan entri yang seharusnya digantikannya kini mungkin nonaktif. Pratinjau lagi."
        }
        "The project entries were written and their withheld values were not; replace them again to retry." => {
            "Entri proyek sudah ditulis, nilai yang ditahan tidak; ganti lagi untuk mencoba ulang."
        }
        "Replaced {id}." => "{id} diganti.",
        "Added {id}." => "{id} ditambahkan.",
        "Delivers to Session {session} branch {branch} ({mode})." => {
            "Mengirim ke Sesi {session} cabang {branch} ({mode})."
        }
        "Launches a new Session in {cwd}." => "Menjalankan Sesi baru di {cwd}.",
        "Disabled: {code} at {instant}." => "Dinonaktifkan: {code} pada {instant}.",
        "The definition changed since it was read; nothing was written." => {
            "Definisi berubah sejak dibaca; tidak ada yang ditulis."
        }
        "Columns: ID, client, scope, path, state, entries or error." => {
            "Kolom: ID, klien, cakupan, path, status, entri atau galat."
        }
        "No value from a store is read out or printed; --from ID previews one inside the daemon." => {
            "Tidak ada nilai dari penyimpanan yang dibaca atau dicetak; --from ID mempratinjau satu di dalam daemon."
        }
        "Columns: ID, action, destination ID.\nA renamed entry is written under its destination id; the source's entry with the old id is left as it was." => {
            "Kolom: ID, aksi, ID tujuan.\nEntri yang diganti namanya ditulis dengan id tujuannya; entri sumber dengan id lama dibiarkan apa adanya."
        }
        "Imported and replaced entries start disabled: a user entry is written with enabled: false, and a project entry has no enablement record until you enable it locally." => {
            "Entri yang diimpor dan diganti mulai nonaktif: entri pengguna ditulis dengan enabled: false, dan entri proyek tidak punya catatan pengaktifan sampai kamu mengaktifkannya secara lokal."
        }
        "This daemon cannot list other clients' MCP stores." => {
            "Daemon ini tidak bisa mendaftar penyimpanan MCP klien lain."
        }
        "This daemon cannot import MCP servers." => "Daemon ini tidak bisa mengimpor server MCP.",
        "The document holds no mcpServers entries." => "Dokumen tidak memuat entri mcpServers.",
        "Columns: ID, status, transport, changed fields, unsupported fields, error.\nA changed field names what a write would change and the kind of value it holds; no value is printed.\nAn unsupported field is kept unused or dropped, as shown after the field." => {
            "Kolom: ID, status, transport, field yang berubah, field tak didukung, galat.\nField yang berubah menyebut apa yang akan diubah oleh penulisan dan jenis nilainya; tidak ada nilai yang dicetak.\nField tak didukung dibiarkan tak terpakai atau dibuang, seperti yang ditampilkan setelah field."
        }
        "A project entry is written as its public skeleton: the workspace file gets the command, declared cwd, URL origin and public OAuth fields. Its arguments, environment and header values, bearer token, a URL with userinfo, path, query or fragment, and unknown members go to your user-scope mcp-secrets record, never to the workspace file." => {
            "Entri proyek ditulis sebagai kerangka publiknya: file ruang kerja mendapat perintah, cwd yang dideklarasikan, asal URL, dan field OAuth publik. Argumen, nilai lingkungan dan header, token bearer, URL dengan userinfo, path, query atau fragmen, serta anggota yang tidak dikenal masuk ke catatan mcp-secrets lingkup pengguna, tidak pernah ke file ruang kerja."
        }
        "{id} has {count} more field names than the lists above show." => {
            "{id} punya {count} nama field lagi yang tidak ditampilkan daftar di atas."
        }
        "Top-level members the import does not carry: none." => {
            "Anggota tingkat atas yang tidak dibawa impor: tidak ada."
        }
        "Top-level members the import does not carry: {names}." => {
            "Anggota tingkat atas yang tidak dibawa impor: {names}."
        }
        "Applying the selection at this revision." => "Menerapkan pilihan pada revisi ini.",
        "An unchanged entry was already in the source and was not written." => {
            "Entri yang tidak berubah sudah ada di sumber dan tidak ditulis."
        }
        _ => return None,
    })
}
