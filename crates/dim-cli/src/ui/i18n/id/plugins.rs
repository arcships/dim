pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Plugins" => "Plugin",
        "Enter on/off · r reload · Ctrl+R refresh · Esc close" => {
            "Enter nyala/mati · r muat ulang · Ctrl+R segarkan · Esc tutup"
        }
        "Locked: Dim needs it, so it cannot be turned off." => {
            "Terkunci: Dim memerlukannya, jadi tidak bisa dimatikan."
        }
        "Loading plugins..." => "Memuat plugin...",
        "No plugins." => "Tidak ada plugin.",
        "Working..." => "Memproses...",
        "locked" => "terkunci",
        "Uses {use}" => "Memakai {use}",
        "Candidates: {candidates}" => "Kandidat: {candidates}",
        "Waiting for: {names}" => "Menunggu: {names}",
        "Refused {kind}: {error}" => "{kind} ditolak: {error}",
        "Turn off {name}?" => "Matikan {name}?",
        "Its tools and content stop in new turns until you turn it on again." => {
            "Tool dan kontennya berhenti di giliran baru sampai Anda menyalakannya lagi."
        }
        "Turn off" => "Matikan",
        "Turned on {name}." => "{name} dinyalakan.",
        "Turned off {name}." => "{name} dimatikan.",
        "Reloaded {name}." => "{name} dimuat ulang.",
        "Installed {name} ({id})." => "{name} ({id}) terpasang.",
        "Installed {name} ({id}); it is not turned on." => {
            "{name} ({id}) terpasang; belum dinyalakan."
        }
        "Installing {path}..." => "Memasang {path}...",
        "Restart required: it loads after the App Server restarts (dimcode daemon stop)." => {
            "Perlu mulai ulang: dimuat setelah App Server dimulai ulang (dimcode daemon stop)."
        }
        "Skipped {kinds}: their owner is not running." => {
            "{kinds} dilewati: pemiliknya tidak berjalan."
        }
        "Usage: /plugins [install PATH]" => "Pemakaian: /plugins [install PATH]",
        "list plugins, turn them on or off, reload (install PATH)" => {
            "daftar plugin, nyalakan atau matikan, muat ulang (install PATH)"
        }
        "Enter open /runtime · o on/off · r reload · Esc close" => {
            "Enter buka /runtime · o nyala/mati · r muat ulang · Esc tutup"
        }
        "waits for the {runtime} runtime" => "menunggu runtime {runtime}",
        _ => return None,
    })
}
