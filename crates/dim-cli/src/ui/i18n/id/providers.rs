pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Refresh model list" => "Segarkan daftar model",
        "Model list refreshed" => "Daftar model disegarkan",
        "Model list refresh failed: {reason}" => "Gagal menyegarkan daftar model: {reason}",
        "Refreshing model list… · esc close" => "Menyegarkan daftar model… · Esc tutup",
        // providers_panel.rs
        "Providers" => "Provider",
        "Enter open · Esc clear/close" => "Enter buka · Esc hapus/tutup",
        "↑↓/Tab move · Enter activate · Esc back" => {
            "↑↓/Tab pindah · Enter aktifkan · Esc kembali"
        }
        "Loading providers..." => "Memuat provider…",
        "No matching providers." => "Tak ada provider cocok.",
        "Custom providers are not available yet" => "Provider kustom belum tersedia",
        "Testing connection..." => "Menguji koneksi…",
        "Connect {providerId}" => "Hubungkan {providerId}",
        "(default)" => "(bawaan)",
        "(required)" => "(wajib)",
        "Test + apply" => "Uji + terapkan",
        "Testing sends the stored credential to this URL." => {
            "Uji mengirim kredensial tersimpan ke URL ini."
        }
        "Disconnect" => "Putuskan",
        "Enable provider" => "Aktifkan provider",
        "Disable provider" => "Nonaktifkan provider",
        "Error: {error}" => "Error: {error}",
        "disabled" => "nonaktif",
        "current" => "saat ini",
        "Sign in" => "Masuk",
        "ready" => "siap",
        "error" => "error",
        "not connected" => "tak terhubung",
        "Add custom provider" => "Tambah provider kustom",
        // oauth_view.rs
        "Checking account…" => "Cek akun…",
        "Not signed in" => "Belum masuk",
        "Signed in as {account}" => "Masuk sebagai {account}",
        "Open models" => "Buka model",
        "View plan" => "Lihat paket",
        "Sign out" => "Keluar",
        "Checking usage…" => "Cek pemakaian…",
        "Usage: {reason}" => "Pemakaian: {reason}",
        "Credits" => "Kredit",
        "unlimited" => "tanpa batas",
        "{hours}h window" => "jendela {hours}j",
        "/month" => "/bulan",
        "/year" => "/tahun",
        "expires {date}" => "berakhir {date}",
        "resets {date}" => "reset {date}",
        "↑↓ move · enter activate · ctrl+r refresh · esc close" => {
            "↑↓ pindah · enter aktifkan · ctrl+r refresh · esc tutup"
        }
        "Login with DimAgent" => "Masuk dengan DimAgent",
        "Enterprise login" => "Masuk enterprise",
        "Device login" => "Masuk perangkat",
        "Email" => "Email",
        "Continue" => "Lanjut",
        "Browser login" => "Masuk via browser",
        "Restart device login" => "Ulangi masuk perangkat",
        "Select to copy · Enter opens link" => "Pilih untuk menyalin · Enter buka tautan",
        "Link shortened · Enter opens link" => "Tautan dipendekkan · Enter buka",
        "Callback URL" => "URL callback",
        "Submit callback" => "Kirim callback",
        "The browser could not be opened. Open the link manually, then return here." => {
            "Browser tak bisa dibuka. Buka tautan manual, lalu kembali ke sini."
        }
        "Waiting for authorization" => "Menunggu otorisasi",
        "{name} device login" => "Masuk perangkat {name}",
        "Open this URL: {url}" => "Buka URL ini: {url}",
        "Enter code:    {code}" => "Masukkan kode: {code}",
        "Waiting after server slow-down" => "Menunggu, server minta melambat",
        "OAuth login cancelled" => "Masuk OAuth dibatalkan",
        "Enter your work email" => "Masukkan email kerja",
        "Invalid OAuth state - possible CSRF attempt" => {
            "State OAuth tak valid - mungkin upaya CSRF"
        }
        "Preparing login... · esc cancel" => "Menyiapkan masuk… · esc batal",
        "Completing login... · esc cancel" => "Menyelesaikan masuk… · esc batal",
        "enter restart · esc back" => "enter ulangi · esc kembali",
        "type value · ↑↓/tab move · enter submit · esc cancel" => {
            "ketik nilai · ↑↓/tab pindah · enter kirim · esc batal"
        }
        "↑↓ move · enter activate · esc back" => "↑↓ pindah · enter aktifkan · esc kembali",
        "↑↓ move · enter activate · esc close" => "↑↓ pindah · enter aktifkan · esc tutup",
        "Model id prefix" => "Prefiks id model",
        "(none)" => "(tidak ada)",
        "identified: {model} at {provider}" => "teridentifikasi: {model} di {provider}",
        "inferred from several rows: {model} at {provider}" => {
            "disimpulkan dari beberapa baris: {model} di {provider}"
        }
        "ambiguous: {candidates}" => "ambigu: {candidates}",
        "unidentified" => "tidak teridentifikasi",
        "Identified {rule}" => "Teridentifikasi {rule}",
        "from the Provider's own list" => "dari daftar milik penyedia",
        "from the whole qualified id" => "dari id terkualifikasi lengkap",
        "by its tokens" => "melalui tokennya",
        "by its canonical id or display name" => "melalui id kanonis atau nama tampilannya",
        "from the custom-provider list" => "dari daftar custom-provider",
        "by merging the rows that name it" => "dengan menggabungkan baris yang menyebutnya",
        "ambiguous" => "ambigu",
        "{field} {value} supplied by {provider}/{model}" => {
            "{field} {value} disediakan oleh {provider}/{model}"
        }
        "(paste here)" => "(tempel di sini)",
        _ => return None,
    })
}
