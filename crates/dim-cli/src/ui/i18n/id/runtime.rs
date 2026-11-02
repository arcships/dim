pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "This daemon does not serve plugin runtimes ({version})" => {
            "Daemon ini tidak menyediakan runtime plugin ({version})"
        }
        "Plugin runtimes" => "Runtime plugin",
        "Enter install or apply · s source · a auto · r retry · Esc close" => {
            "Enter pasang atau terapkan · s sumber · a otomatis · r coba lagi · Esc tutup"
        }
        "Esc close · the acquisition keeps running if you close this" => {
            "Esc tutup · pengambilan tetap berjalan walau panel ditutup"
        }
        "Enter retry · s switch {name} to {source} · Esc close" => {
            "Enter coba lagi · s ganti {name} ke {source} · Esc tutup"
        }
        "Loading plugin runtimes..." => "Memuat runtime plugin...",
        "acquiring" => "mengambil",
        "starting" => "memulai",
        "down" => "mati",
        "1 plugin waits for {runtime}: {names}" => "1 plugin menunggu runtime {runtime}: {names}",
        "{count} plugins wait for {runtime}: {names}" => {
            "{count} plugin menunggu runtime {runtime}: {names}"
        }
        "show, install or configure the plugin runtimes" => {
            "lihat, pasang, atau atur runtime plugin"
        }
        "Usage: /runtime [install NAME | source NAME download|system | auto NAME on|off]" => {
            "Penggunaan: /runtime [install NAMA | source NAMA download|system | auto NAMA on|off]"
        }
        "Enter install or apply · s source · r retry · Esc close" => {
            "Enter pasang atau terapkan · s sumber · r coba lagi · Esc tutup"
        }
        "auto on" => "otomatis nyala",
        "auto off" => "otomatis mati",
        _ => return None,
    })
}
