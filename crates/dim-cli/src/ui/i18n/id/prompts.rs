pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        // prompts.rs: exit confirmation
        "The agent is still running." => "Agen masih berjalan.",
        "AskUser is waiting for an answer." => "AskUser menunggu jawaban.",
        "A permission request is waiting." => "Permintaan izin menunggu.",
        "Background work may still be active." => "Tugas latar mungkin masih aktif.",
        "Exit dimcode?" => "Keluar dimcode?",
        "Exit" => "Keluar",
        "Stay" => "Tetap",
        "y exit · n stay · ←→ move · enter confirm" => {
            "y keluar · n tetap · ←→ pindah · enter konfirmasi"
        }
        "Restart dimcode?" => "Mulai ulang dimcode?",
        "Restart" => "Mulai ulang",
        "y restart · n stay · ←→ move · enter confirm" => {
            "y mulai ulang · n tetap · ←→ pindah · enter konfirmasi"
        }
        // upgrade_dialog.rs
        "Update available" => "Pembaruan tersedia",
        "Version {latest} is available (you have {current})." => {
            "Versi {latest} tersedia (terpasang {current})."
        }
        "Will run: {command}" => "Akan jalankan: {command}",
        "To update, run: {command}" => "Perbarui dengan: {command}",
        "Update now" => "Perbarui",
        "Skip this version" => "Lewati versi ini",
        "Skipped {version}." => "{version} dilewati.",
        "Later" => "Nanti",
        "Close" => "Tutup",
        "Restart now" => "Mulai ulang",
        "Updating to {version}..." => "Memperbarui ke {version}…",
        "Updated to {version}. Restart DimCode to use it." => {
            "Diperbarui ke {version}. Mulai ulang DimCode untuk memakai."
        }
        "Update failed: {error}" => "Pembaruan gagal: {error}",
        "←→ move · Enter confirm · Esc close" => "←→ pindah · Enter konfirmasi · Esc tutup",
        // prompts.rs: permission request
        "Permission required" => "Perlu izin",
        "Allow once" => "Izinkan sekali",
        "Allow always" => "Selalu izinkan",
        "Reject" => "Tolak",
        "Approve for me" => "Setujui untuk saya",
        "Full access" => "Akses penuh",
        "Run {tool}" => "Jalankan {tool}",
        "Changing the approvals mode from here is not yet available" => {
            "Ubah mode persetujuan di sini belum tersedia"
        }
        "y allow · n reject · ←→/Tab move · Enter confirm" => {
            "y izinkan · n tolak · ←→/Tab pindah · Enter konfirmasi"
        }
        "Enter submit · Tab edit · Esc decline" => "Enter kirim · Tab edit · Esc tolak",
        "Space toggle · Tab next · Esc decline" => "Space centang · Tab lanjut · Esc tolak",
        "1-9 select · Tab next · Esc decline" => "1-9 pilih · Tab lanjut · Esc tolak",
        "Question" => "Pertanyaan",
        "Question on branch {branch}" => "Pertanyaan dari cabang {branch}",
        "Permission on branch {branch}" => "Izin pada cabang {branch}",
        "Sending answer…" => "Mengirim jawaban…",
        "Question not ready; press Enter to send again" => {
            "Pertanyaan belum siap; Enter untuk kirim lagi"
        }
        // prompts.rs: AskUser
        "Confirm" => "Konfirmasi",
        "Enter save · Esc back" => "Enter simpan · Esc kembali",
        "Enter submit · 1-9 edit · Tab/←→ back · Esc decline" => {
            "Enter kirim · 1-9 edit · Tab/←→ kembali · Esc tolak"
        }
        "↑↓ move · Space toggle · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ pindah · Space centang · Enter lanjut · Tab/←→ ganti · Esc tolak"
        }
        "↑↓ move · 1-9 select · Enter next · Tab/←→ switch · Esc decline" => {
            "↑↓ pindah · 1-9 pilih · Enter lanjut · Tab/←→ ganti · Esc tolak"
        }
        "AskUserQuestion received an invalid payload." => "AskUserQuestion: payload tak valid.",
        "Missing answer for Q{index}" => "P{index} belum dijawab",
        "Type your own answer" => "Ketik jawaban sendiri",
        "Review your answers and press Enter to submit." => {
            "Tinjau jawaban lalu Enter untuk kirim."
        }
        "Not answered" => "Belum dijawab",
        // approvals_panel.rs
        "Tool Approvals" => "Persetujuan Tool",
        "full access" => "akses penuh",
        "All tools run automatically, bypass permissions" => {
            "Semua tool jalan otomatis, lewati izin"
        }
        "approve for me" => "setujui untuk saya",
        "Agent automatically reviews commands and tool actions. Unsafe commands ask for confirmation; routine actions pass automatically, safe, smart, and convenient." => {
            "Agen otomatis meninjau perintah dan aksi tool. Perintah tak aman minta konfirmasi; aksi rutin lolos otomatis: aman, cerdas, praktis."
        }
        "Default permissions" => "Izin bawaan",
        "default" => "bawaan",
        "Read tools run automatically, write tools require confirmation" => {
            "Tool baca otomatis, tool tulis perlu konfirmasi"
        }
        "current" => "saat ini",
        "↑↓/Tab move · Enter apply · Esc back" => {
            "↑↓/Tab pindah · Enter terapkan · Esc kembali"
        }
        "Confirm Full Access" => "Konfirmasi Akses Penuh",
        "Cancel" => "Batal",
        "Tab switch  Enter confirm  Esc cancel" => "Tab ganti  Enter konfirmasi  Esc batal",
        "Full access reduces confirmation prompts and can run sensitive actions, file changes, or external commands directly. Use it only when you trust the current task." => {
            "Akses penuh mengurangi konfirmasi dan bisa langsung menjalankan aksi sensitif, ubah file, atau perintah eksternal. Pakai hanya jika Anda percaya tugas ini."
        }
        // transcript/tool_group.rs: summary verbs
        "ran" => "menjalankan",
        "running" => "sedang menjalankan",
        "read" => "membaca",
        "reading" => "sedang membaca",
        "explored" => "menjelajahi",
        "exploring" => "sedang menjelajahi",
        "edited" => "mengedit",
        "editing" => "sedang mengedit",
        "changed" => "mengubah",
        "changing" => "sedang mengubah",
        "checked on" => "memeriksa",
        "checking on" => "sedang memeriksa",
        "Explored" => "Menjelajahi",
        "Exploring" => "Sedang menjelajahi",
        "Edited" => "Mengedit",
        "Editing" => "Sedang mengedit",
        "Changed" => "Mengubah",
        "Changing" => "Sedang mengubah",
        // transcript/tool_group.rs: counts
        "1 file" => "1 file",
        "{count} files" => "{count} file",
        "1 web page" => "1 halaman web",
        "{count} web pages" => "{count} halaman web",
        "1 skill" => "1 skill",
        "{count} skills" => "{count} skill",
        "1 search" => "1 pencarian",
        "{count} searches" => "{count} pencarian",
        "1 command" => "1 perintah",
        "{count} commands" => "{count} perintah",
        "1 task" => "1 tugas",
        "{count} tasks" => "{count} tugas",
        "1 tool" => "1 tool",
        "{count} tools" => "{count} tool",
        "{files} and {pages}" => "{files} dan {pages}",
        ", " => ", ",
        " · {count} failed" => " · {count} gagal",
        // transcript/tool_group.rs: call rows
        "Read" => "Baca",
        "List" => "Daftar",
        "Grep" => "Grep",
        "Search the web" => "Cari di web",
        "Read skill" => "Baca skill",
        "Search git history" => "Cari riwayat git",
        "Edit" => "Edit",
        "Write" => "Tulis",
        "Create" => "Buat",
        " (detached)" => " (latar)",
        "Checked on background task" => "Memeriksa tugas latar",
        "Waiting…" => "Menunggu…",
        "… +{count} lines" => "… +{count} baris",
        _ => return None,
    })
}
