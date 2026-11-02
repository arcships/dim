pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // tui.rs
        "Enter output / interrupt member · x stop command · Esc close" => {
            "Enter output / interupsi anggota · x stop perintah · Esc tutup"
        }
        "Enter output / interrupt member · x stop command · Ctrl+X stop all · Esc close" => {
            "Enter output / interupsi anggota · x stop perintah · Ctrl+X stop semua · Esc tutup"
        }
        "Loading branch history... Stop-all unavailable" => {
            "Memuat riwayat cabang… Stop-semua tak tersedia"
        }
        "Branch history failed; reopen to retry" => "Riwayat cabang gagal; coba buka ulang",
        "Branch history is incomplete; stop-all unavailable" => {
            "Riwayat cabang belum lengkap; stop-semua tak tersedia"
        }
        "Running" => "Berjalan",
        "Lines" => "Jalur",
        "Enter show the line · x end it · Esc close" => {
            "Enter lihat jalur · x hentikan · Esc tutup"
        }
        "This session has no lines." => "Sesi ini belum punya jalur.",
        "No matching lines." => "Tidak ada jalur yang cocok.",
        "the session itself" => "sesi itu sendiri",
        "working" => "bekerja",
        "ended itself" => "berakhir sendiri",
        "ended by an operation" => "diakhiri operasi",
        "ended with {id}" => "diakhiri bersama {id}",
        "creation failed" => "pembuatan gagal",
        "shown" => "ditampilkan",
        "line" => "jalur",
        "line {id}" => "jalur {id}",
        "jump to a message of the shown line" => "lompat ke pesan di jalur yang tampil",
        "Timeline" => "Linimasa",
        "Enter jump · e edit · q quote · b branch · f fork · Esc close" => {
            "Enter lompat · e ubah · q kutip · b cabang · f fork · Esc tutup"
        }
        "No messages yet." => "Belum ada pesan.",
        "Already on screen above; /tui jumps to it" => "Sudah ada di atas; /tui langsung lompat",
        "{count} earlier messages · Enter or click to load" => {
            "{count} pesan lama · Enter atau klik untuk memuat"
        }
        "follow, talk to and end this session's lines" => {
            "ikuti, ajak bicara, dan akhiri jalur sesi ini"
        }
        "End line" => "Akhiri jalur",
        "y/N" => "y/N",
        "main is the session itself; a line ends, main does not" => {
            "main adalah sesi itu sendiri; jalur bisa berakhir, main tidak"
        }
        "Line has ended:" => "Jalur sudah berakhir:",
        "Esc close" => "Esc tutup",
        "/{name} needs {argument}" => "/{name} memerlukan {argument}",
        "/{name} takes text only; remove {label} to send" => {
            "/{name} hanya menerima teks; hapus {label} untuk mengirim"
        }
        "e edit · b branch · f fork" => "e ubah · b cabang · f fork",
        "e edit" => "e ubah",
        "Fork the session before “{message}”? (y/N)" => "Fork sesi sebelum “{message}”? (y/N)",
        "Fork the session after the last turn? (y/N)" => {
            "Fork sesi setelah giliran terakhir? (y/N)"
        }
        "⑂ new line from {id} · Esc cancel" => "⑂ jalur baru dari {id} · Esc batal",
        "A fork copies main; b opens a line here" => {
            "Fork hanya menyalin main; b membuka jalur di sini"
        }
        "A line opens from text; images and files were not included" => {
            "Jalur dibuka dengan teks; gambar dan file tidak disertakan"
        }
        "No finished turn to cut at yet" => "Belum ada giliran selesai untuk dipotong",
        "forked {id} · {count} messages" => "fork {id} · {count} pesan",
        "after message {count}" => "setelah pesan {count}",
        "open a line from the end of the shown line" => {
            "buka jalur dari akhir jalur yang ditampilkan"
        }
        "fork the session at its last finished turn" => {
            "fork sesi pada giliran terakhir yang selesai"
        }
        "rename this session" => "ganti nama sesi ini",
        "Renamed to {title}" => "Diganti namanya menjadi {title}",
        "The Session is now untitled." => "Sesi ini sekarang tanpa judul.",
        "This daemon does not support renaming Sessions" => {
            "Daemon ini tidak mendukung penggantian nama Sesi"
        }
        "Renaming failed" => "Penggantian nama gagal",
        "Starting" => "Memulai",
        "Delegation result" => "Hasil delegasi",
        "Enter details · x stop selected · Ctrl+X stop all · Esc close" => {
            "Enter detail · x stop terpilih · Ctrl+X stop semua · Esc tutup"
        }
        "x interrupt member · Esc back" => "x interupsi anggota · Esc kembali",
        "read-only" => "hanya-baca",
        "Member Session" => "Sesi Anggota",
        "Loading conversation..." => "Memuat percakapan…",
        "No conversation yet" => "Belum ada percakapan",
        "Observing conversation" => "Mengamati percakapan",
        "Conversation unavailable" => "Percakapan tak tersedia",
        "Session closed" => "Sesi ditutup",
        "Member conversation" => "Percakapan anggota",
        "x stop command · Esc back" => "x stop perintah · Esc kembali",
        "Esc back" => "Esc kembali",
        "Tail preview - last 2,000 lines / 50 KiB" => "Pratinjau akhir - 2.000 baris / 50 KiB",
        "Loading output..." => "Memuat output…",
        "No output yet" => "Belum ada output",
        "Live output unavailable" => "Output langsung tak tersedia",
        "Live output unavailable in this profile" => "Output langsung tak tersedia di profil ini",
        "Output read failed" => "Gagal baca output",
        "Completed" => "Selesai",
        "Failed" => "Gagal",
        "Cancelled" => "Dibatalkan",
        "workflow" => "alur kerja",
        "Stopping" => "Menghentikan",
        "Stopped" => "Dihentikan",
        "Interrupted" => "Diinterupsi",
        "last observed" => "terakhir diamati",
        "Loading Workflow runs... Stop-all unavailable" => {
            "Memuat run Workflow… Stop-semua tak tersedia"
        }
        "Workflow: last observed; refresh failed, reopen to retry" => {
            "Workflow: terakhir diamati; refresh gagal, coba buka ulang"
        }
        "Workflow runs unavailable; reopen to retry" => {
            "Run Workflow tak tersedia; coba buka ulang"
        }
        "The selected Workflow run is no longer available" => "Run Workflow terpilih sudah tak ada",
        "Workflow inventory is incomplete; stop-all unavailable" => {
            "Daftar Workflow belum lengkap; stop-semua tak tersedia"
        }
        "Workflow stop unavailable" => "Stop Workflow tak tersedia",
        "Run no longer available; last observed record" => {
            "Run sudah tak ada; catatan terakhir diamati"
        }
        "x stop workflow · Esc back" => "x stop workflow · Esc kembali",
        "Run" => "Run",
        "Source" => "Sumber",
        "Stages" => "Tahap",
        "result" => "hasil",
        "Log" => "Log",
        "Report" => "Laporan",
        "waiting" => "menunggu",
        "launching" => "memulai",
        "completed" => "selesai",
        "failed" => "gagal",
        "stopped" => "dihentikan",
        "skipped" => "dilewati",
        "lost" => "hilang",
        "none" => "tak ada",
        "pending" => "tertunda",
        "delivered" => "terkirim",
        "undeliverable" => "tak terkirim",
        "Agent is starting; no member Session to stop yet" => {
            "Agen memulai; belum ada Sesi anggota untuk dihentikan"
        }
        "Provider members are controlled by the leader; no stop here" => {
            "Anggota provider diatur pemimpin; tak bisa stop di sini"
        }
        "Provider members have no conversation to open here" => {
            "Anggota provider tak punya percakapan di sini"
        }
        "That delegation is no longer in this transcript" => {
            "Delegasi itu sudah tak ada di transkrip ini"
        }
        "Member model" => "Model anggota",
        "No model observed" => "Model belum teramati",
        "Observing member..." => "Mengamati anggota…",
        "{model} (observation unavailable)" => "{model} (pengamatan tak tersedia)",
        "{model} (Session closed)" => "{model} (Sesi ditutup)",
        "{model} (last observed)" => "{model} (terakhir diamati)",
        "Agent: {name}" => "Agen: {name}",
        "Provider member: {name}" => "Anggota provider: {name}",
        "Task: {task}" => "Tugas: {task}",
        "Activity: {activity}" => "Aktivitas: {activity}",
        "Activity: not observed on this attachment" => "Aktivitas: tak teramati di koneksi ini",
        "Result: {result}" => "Hasil: {result}",
        "Controlled by the leader" => "Diatur pemimpin",
        "› Open member conversation" => "› Buka percakapan anggota",
        "Not started" => "Belum dimulai",
        "Reason: {reason}" => "Alasan: {reason}",
        "Copied to clipboard" => "Disalin ke clipboard",
        "Opened link" => "Tautan dibuka",
        "Link: {url}" => "Tautan: {url}",
        "Not available" => "Tak tersedia",
        "files changed  {count}" => "file diubah  {count}",
        "active session" => "sesi aktif",
        "new-session draft" => "draf sesi baru",
        "Tool approvals: {title} ({target})" => "Persetujuan tool: {title} ({target})",
        "Disconnected · draft kept" => "Terputus · draf disimpan",
        "The App Server closed this interface: {reason}" => {
            "App Server menutup antarmuka ini: {reason}"
        }
        "Disconnected; draft kept. Exit and resume this Session." => {
            "Terputus; draf disimpan. Keluar lalu lanjutkan Sesi ini."
        }
        "Disconnected; draft kept. Exit and run: {command}" => {
            "Terputus; draf disimpan. Keluar lalu jalankan: {command}"
        }
        "Disconnected; command outcome unknown" => "Terputus; hasil perintah tak diketahui",
        "Message queued" => "Pesan diantrekan",
        "Run a shell command · Esc to exit" => "Jalankan perintah shell · Esc keluar",
        "! shell mode" => "! mode shell",
        "!! shell mode · not sent to the model" => "!! mode shell · tak dikirim ke model",
        "Leave shell mode to quote a message" => "Keluar mode shell untuk kutip pesan",
        "A shell command is already running" => "Perintah shell masih berjalan",
        "Running… · Ctrl+C to interrupt · {seconds}s" => {
            "Berjalan… · Ctrl+C interupsi · {seconds}s"
        }
        "exit code {code}" => "kode keluar {code}",
        "killed by {signal}" => "dihentikan oleh {signal}",
        "interrupted" => "diinterupsi",
        "done" => "selesai",
        "failed: {reason}" => "gagal: {reason}",
        "Shell output was not recorded: {reason}" => "Output shell tak tercatat: {reason}",
        "Shell: {reason}" => "Shell: {reason}",
        "Reading clipboard…" => "Membaca clipboard…",
        "Reading image…" => "Membaca gambar…",
        "Wait for the image insertion before sending" => {
            "Tunggu penyisipan gambar sebelum mengirim"
        }
        "The image could not be read" => "Gambar tidak dapat dibaca",
        "An image is still being inserted; paste again when it is in" => {
            "Gambar masih disisipkan; tempel lagi setelah selesai"
        }
        "The clipboard is still being inserted; paste again when it is in" => {
            "Clipboard masih disisipkan; tempel lagi setelah selesai"
        }
        "Clipboard: {reason}" => "Clipboard: {reason}",
        "Wait for clipboard insertion before sending" => {
            "Tunggu clipboard disisipkan sebelum kirim"
        }
        "A submission accepts at most 20 images" => "Maks. 20 gambar per kiriman",
        "first frame" => "hanya frame pertama",
        "re-encoded" => "dikodekan ulang",
        "The current model does not accept images; remove {label} to send" => {
            "Model ini tak menerima gambar; hapus {label} untuk kirim"
        }
        "Files" => "File",
        "Searching…" => "Mencari…",
        "↑↓ select · enter/tab insert · esc close" => {
            "↑↓ pilih · enter/tab sisip · esc tutup"
        }
        "not sent to the model" => "tak dikirim ke model",
        "… {count} more lines" => "… {count} baris lagi",
        "… {count} earlier lines" => "… {count} baris sebelumnya",
        "Interrupt requested; Ctrl+C again to exit" => {
            "Interupsi diminta; Ctrl+C lagi untuk keluar"
        }
        "Press Ctrl+C again to exit" => "Ctrl+C lagi untuk keluar",
        "User interrupted the run." => "Run diinterupsi pengguna.",
        "Could not save the language: {error}" => "Gagal simpan bahasa: {error}",
        "Language set to {locale}" => "Bahasa diatur ke {locale}",
        "Language set to system default" => "Bahasa diatur ke bawaan sistem",
        "Connected {provider}" => "{provider} terhubung",
        "Opened {url}" => "Membuka {url}",
        "Tool details not saved: {error}" => "Detail tool tak disimpan: {error}",
        "/{name} needs a session — send a message first" => {
            "/{name} perlu sesi — kirim pesan dulu"
        }
        "Nothing is running" => "Tak ada yang berjalan",
        "No queued message" => "Antrean kosong",
        "Only in fullscreen (/tui)" => "Hanya di layar penuh (/tui)",
        "new session" => "sesi baru",
        "resumed {id} · {count} messages" => "lanjut {id} · {count} pesan",
        " · earlier history: /timeline" => " · riwayat lama: /timeline",
        "Error: {message}" => "Error: {message}",
        "working…  {elapsed}" => "bekerja…  {elapsed}",
        "thinking…  {elapsed}" => "berpikir…  {elapsed}",
        "Ask anything..." => "Tanya apa saja…",
        "Earlier history · {id} · {count} messages" => "Riwayat lama · {id} · {count} pesan",
        "No active session" => "Tak ada sesi aktif",
        "answer required" => "perlu jawaban",
        "permission required" => "perlu izin",
        "esc interrupt" => "esc interupsi",
        "shift+← edit last · ctrl+s steer now" => "shift+← edit terakhir · ctrl+s steer kini",
        "+{count} more · shift+← edit last · ctrl+s steer now" => {
            "+{count} lagi · shift+← edit terakhir · ctrl+s steer kini"
        }
        "Queue ({count})" => "Antrean ({count})",
        "Thinking…" => "Berpikir…",
        "Thought" => "Berpikir",
        "Thought for {duration}" => "Berpikir {duration}",
        "Welcome to DIM {version}!" => "Selamat datang di DIM {version}!",
        "Type / for commands · Ctrl+O for the palette" => "Ketik / untuk perintah · Ctrl+O palet",
        "Workspace" => "Workspace",
        "Git branch" => "Cabang Git",
        "Bye!" => "Sampai jumpa!",
        "DIM Web is ready" => "DIM Web siap",
        "Opened in your browser" => "Dibuka di browser",
        "Open the sign-in link below" => "Buka tautan masuk di bawah",
        "Web" => "Web",
        "Sign-in link:" => "Tautan masuk:",
        "Resume" => "Lanjutkan",
        "Press Ctrl+C to stop the Web workspace." => "Ctrl+C untuk menghentikan workspace Web.",
        "To resume this session:" => "Lanjutkan sesi ini:",

        // composer/commands.rs: categories
        "session" => "sesi",
        "run" => "run",
        "view" => "tampilan",
        "model" => "model",
        "app" => "aplikasi",
        "skill" => "skill",
        // composer/commands.rs: descriptions
        "start a new session" => "mulai sesi baru",
        "show or set the reasoning effort" => "lihat/atur tingkat penalaran",
        "show or set the priority service tier (on|off)" => {
            "lihat/atur tier layanan prioritas (on|off)"
        }
        "list agents or switch this session's agent" => "daftar agen atau ganti agen sesi",
        "list MCP servers, sign in or out (login|logout ID)" => {
            "daftar server MCP, masuk/keluar (login|logout ID)"
        }
        "resume a previous session" => "lanjutkan sesi sebelumnya",
        "open this session in the browser" => "buka sesi ini di browser",
        "show the context window usage" => "lihat pemakaian jendela konteks",
        "interrupt the run" => "interupsi run",
        "steer with the next queued message" => "steer dengan pesan antrean berikutnya",
        "scroll to the top" => "gulir ke atas",
        "scroll to the bottom" => "gulir ke bawah",
        "pick an earlier message to edit or quote" => "pilih pesan lama untuk diedit/dikutip",
        "collapse or expand tool details, thoughts and compaction summaries" => {
            "lipat/buka detail tool, pikiran, dan ringkasan kompresi"
        }
        "switch between fullscreen and scrollback" => "ganti layar penuh/scrollback",
        "switch theme" => "ganti tema",
        "switch language" => "ganti bahasa",
        "select a model" => "pilih model",
        "connect a provider" => "hubungkan provider",
        "sign in to DimAgent" => "masuk ke DimAgent",
        "DimAgent account · usage · sign out" => "akun DimAgent · pemakaian · keluar",
        "configure tool approvals" => "atur persetujuan tool",
        "choose ask or full access" => "pilih tanya atau akses penuh",
        "open settings" => "buka pengaturan",
        "exit" => "keluar",

        // composer/palette.rs
        "Enter select · ←/→ effort · Ctrl+X clear saved · Esc clear/close" => {
            "Enter pilih · ←/→ upaya · Ctrl+X hapus simpanan · Esc hapus/tutup"
        }
        "Enter select · Esc clear/close" => "Enter pilih · Esc hapus/tutup",
        "esc close" => "esc tutup",
        "Suggestions" => "Saran",
        "Skills" => "Skill",
        "Commands" => "Perintah",
        "Command Palette" => "Palet Perintah",
        "No matches" => "Tak ada hasil",
        " (unavailable)" => " (tak tersedia)",

        // composer/mod.rs
        "[Text: {preview} ({count} chars)]" => "[Teks: {preview} ({count} karakter)]",
        "[... {count} lines]" => "[… {count} baris]",
        "pasted text" => "teks tempelan",

        // list_panel.rs

        // settings_panel.rs
        "on" => "aktif",
        "off" => "nonaktif",
        "Interface" => "Antarmuka",
        "Theme" => "Tema",
        "Language" => "Bahasa",
        "Startup UI form" => "Bentuk UI awal",
        "fullscreen" => "layar penuh",
        "scrollback" => "scrollback",
        "Tool details" => "Detail tool",
        "expanded" => "terbuka",
        "collapsed" => "terlipat",
        "Mouse (fullscreen)" => "Mouse (layar penuh)",
        "Permissions" => "Izin",
        "Tool approvals" => "Persetujuan tool",
        "Execution" => "Eksekusi",
        "Models" => "Model",
        "Settings" => "Pengaturan",
        "↑↓ move · Enter select · ←→/Space change · Esc close" => {
            "↑↓ pindah · Enter pilih · ←→/Space ubah · Esc tutup"
        }

        // language_panel.rs
        "↑↓ preview · Enter save · Esc restore" => {
            "↑↓ pratinjau · Enter simpan · Esc pulihkan"
        }
        "System default ({locale})" => "Bawaan sistem ({locale})",

        // theme_panel.rs
        "↑↓ preview · Enter apply · Esc cancel" => {
            "↑↓ pratinjau · Enter terapkan · Esc batal"
        }
        "System" => "Sistem",
        "Could not save the theme: {error}" => "Tidak bisa menyimpan tema: {error}",

        // sessions_panel.rs
        "Sessions" => "Sesi",
        "Enter open · Ctrl+R refresh · Esc close" => "Enter buka · Ctrl+R refresh · Esc tutup",
        "Loading sessions..." => "Memuat sesi…",
        "No saved sessions yet." => "Belum ada sesi tersimpan.",
        "No matching sessions." => "Tak ada sesi cocok.",
        "now" => "baru saja",
        "{count}m ago" => "{count}mnt lalu",
        "{count}h ago" => "{count}j lalu",
        "{count}d ago" => "{count}hr lalu",

        // models_panel.rs
        "Select a model" => "Pilih model",
        "Loading models..." => "Memuat model…",
        "No models available. Connect a provider first." => {
            "Tak ada model. Hubungkan provider dulu."
        }
        "No matching models." => "Tak ada model cocok.",
        "current" => "saat ini",
        "Edit models" => "Edit model",
        "Editing models is not available yet" => "Edit model belum tersedia",
        "{provider}: 1 model unavailable · dimcode models {id}" => {
            "{provider}: 1 model tak tersedia · dimcode models {id}"
        }
        "{provider}: {count} models unavailable · dimcode models {id}" => {
            "{provider}: {count} model tak tersedia · dimcode models {id}"
        }
        "{provider}: 1 model needs a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: 1 model membutuhkan jendela · dimcode models {id} --set-missing-limits"
        }
        "{provider}: {count} models need a window · dimcode models {id} --set-missing-limits" => {
            "{provider}: {count} model membutuhkan jendela · dimcode models {id} --set-missing-limits"
        }
        "{provider}: no models · dimcode models {id} --refresh" => {
            "{provider}: tanpa model · dimcode models {id} --refresh"
        }
        "{provider}: no models · add a model with provider.models.create" => {
            "{provider}: tanpa model · tambahkan model dengan provider.models.create"
        }
        "{provider}: {unavailable} unavailable and {disabled} disabled · dimcode models {id}" => {
            "{provider}: {unavailable} tidak tersedia dan {disabled} dinonaktifkan · dimcode models {id}"
        }
        "{provider}: catalog unreadable · dimcode models {id}" => {
            "{provider}: katalog tidak terbaca · dimcode models {id}"
        }
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N." => {
            "Model yang batasnya belum dideklarasikan tidak dapat dipilih; deklarasikan dengan dimcode models {id} --set-missing-limits --context-window N --max-output-tokens N."
        }
        "Declare them with dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N." => {
            "Deklarasikan dengan dimcode models {provider} --set-limits {model} --context-window N --max-output-tokens N."
        }
        "No cached catalog. Sign in and run models PROVIDER --refresh." => {
            "Tidak ada katalog tersimpan. Masuk dan jalankan models PROVIDER --refresh."
        }
        "Saved model preferences cleared" => "Preferensi model tersimpan dihapus",
        "{provider}: 1 model disabled · dimcode models {id}" => {
            "{provider}: 1 model dinonaktifkan · dimcode models {id}"
        }
        "{provider}: {count} models disabled · dimcode models {id}" => {
            "{provider}: {count} model dinonaktifkan · dimcode models {id}"
        }
        "No models. Run dimcode models {id} --refresh to list them." => {
            "Tidak ada model. Jalankan dimcode models {id} --refresh untuk menampilkannya."
        }
        "No models. The provider listed none." => {
            "Tidak ada model. Penyedia tidak menampilkan satu pun."
        }
        "Enter select · Esc back" => "Enter pilih · Esc kembali",
        "Subagent models" => "Model subagen",
        "Subagent models: {policy}" => "Model subagen: {policy}",
        "Fixed subagent model" => "Model subagen tetap",
        "Agent choice (default)" => "Pilihan agen (bawaan)",
        "Agent choice" => "Pilihan agen",
        "Inherit requesting branch" => "Warisi cabang peminta",
        "Fixed model" => "Model tetap",
        "Applies to new members, including named Agents." => {
            "Untuk anggota baru, termasuk Agen bernama."
        }
        "Existing members keep their models." => "Anggota lama tetap pakai modelnya.",
        "unknown" => "tak diketahui",
        "Subagent model policy saved" => "Kebijakan model subagen disimpan",
        "Subagent model policy changed elsewhere; choose again to reread it" => {
            "Kebijakan model subagen diubah di tempat lain; pilih lagi untuk muat ulang"
        }
        "Subagent model policy: {error}" => "Kebijakan model subagen: {error}",
        "drawing diagram… {count} lines" => "menggambar diagram… {count} baris",
        // notify.rs
        "Task complete" => "Tugas selesai",
        "Permission needed: {tool}" => "Perlu izin: {tool}",
        "Question waiting for your answer" => "Pertanyaan menunggu jawaban",
        "Notify when agent needs you" => "Beri tahu saat agen butuh Anda",
        "Show ACP sessions" => "Tampilkan sesi ACP",
        // keys.rs
        "show the key bindings" => "lihat pintasan tombol",
        "Application keys" => "Tombol aplikasi",
        "Enter submit or queue · empty draft: steer first queued · Shift+Enter or Ctrl+J newline · ↑↓ history" => {
            "Enter kirim atau antrekan · draf kosong: steer antrean pertama · Shift+Enter/Ctrl+J baris baru · ↑↓ riwayat"
        }
        "Ctrl+O or Ctrl+P commands · Esc interrupt, queued messages return · Ctrl+S steer draft or first queued · Shift+← edit last queued" => {
            "Ctrl+O/Ctrl+P perintah · Esc interupsi, pesan antrean kembali · Ctrl+S steer draf atau antrean pertama · Shift+← edit antrean terakhir"
        }
        "Ctrl+C clear, twice to exit · Ctrl+D exit on an empty draft" => {
            "Ctrl+C hapus, 2x untuk keluar · Ctrl+D keluar jika draf kosong"
        }
        "Fullscreen" => "Layar penuh",
        "PageUp/PageDown scroll · Home/End top or bottom (empty draft) · Tab pick a message, e edit it, q quote it" => {
            "PageUp/PageDown gulir · Home/End atas/bawah (saat draf kosong) · Tab pilih pesan, e edit, q kutip"
        }
        "wheel scroll · drag select and copy · click a link to open it, a tool group to expand it" => {
            "roda gulir · seret pilih dan salin · klik tautan/grup tool untuk membukanya"
        }
        "mouse off (/settings): the terminal selects and copies" => {
            "mouse nonaktif (/settings): terminal yang pilih dan salin"
        }
        "Terminal-owned history" => "Riwayat milik terminal",
        "scroll: {value}" => "gulir: {value}",
        "search: {value}" => "cari: {value}",
        "copy: {value}" => "salin: {value}",
        "terminal search" => "cari di terminal",
        "terminal scrollback" => "scrollback terminal",
        "terminal copy" => "salin terminal",
        "drag to select" => "seret untuk pilih",
        "drag or {shortcut}" => "seret atau {shortcut}",
        "Compacting context…" => "Mengompresi konteks…",
        "Context compacted" => "Konteks dikompresi",
        "Context compaction skipped" => "Kompresi konteks dilewati",
        "Context compaction failed" => "Kompresi konteks gagal",
        "Context compaction cancelled" => "Kompresi konteks dibatalkan",
        "near the compaction threshold" => "mendekati ambang kompresi",
        "This daemon does not support context compaction" => {
            "Daemon ini tidak mendukung kompresi konteks"
        }
        "compact the context of the shown line" => "kompresi konteks baris yang ditampilkan",
        "follow or change the shown line's goal" => "ikuti atau ubah goal baris yang ditampilkan",
        "◎ goal active" => "◎ goal aktif",
        "◎ goal paused" => "◎ goal dijeda",
        "Kept verbatim" => "Dipertahankan apa adanya",
        "Tool effects" => "Catatan operasi alat",
        "insufficient space: {parts}" => "ruang tidak cukup: {parts}",
        "system prompt" => "system prompt",
        "tools" => "alat",
        "appended messages" => "pesan tambahan",
        "keyed baseline" => "baseline ber-kunci",
        "pinned inputs" => "input yang dipatok",
        "digest" => "digest",
        "summary" => "ringkasan",
        _ => return None,
    })
}
