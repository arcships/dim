//! Indonesian model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Conversation state could not be read; retry the read before submitting" => {
            "Status percakapan tidak dapat dibaca. Muat ulang sebelum mengirim"
        }
        "The centre moved before the command ran; the draft was kept" => {
            "Sesi atau cabang berubah sebelum pengiriman. Draf tetap tersimpan"
        }
        "The selected device restarted during file search" => {
            "Perangkat yang dipilih memulai ulang saat mencari berkas"
        }
        "Target does not support file completion; type the remote path" => {
            "Tujuan tidak mendukung pelengkapan berkas. Masukkan jalur jarak jauh"
        }
        "Select a regular file to upload" => "Pilih berkas biasa untuk diunggah",
        "Target did not return an image" => "Tujuan tidak mengembalikan gambar",
        "File search interrupted" => "Pencarian berkas terputus",
        "Target changed during attachment preparation" => "Tujuan berubah saat menyiapkan lampiran",
        "Attachment preparation cancelled" => "Penyiapan lampiran dibatalkan",
        "The session or line changed during upload; the draft was kept" => {
            "Sesi atau alur berubah saat mengunggah. Draf disimpan"
        }
        "Preparing attachments · Ctrl+C to cancel" => {
            "Menyiapkan lampiran · Ctrl+C untuk membatalkan"
        }
        "{reason}; draft retained" => "{reason}. Draf disimpan",
        "The previous attachment submission is pending; draft retained" => {
            "Pengiriman lampiran sebelumnya masih tertunda. Draf disimpan"
        }
        "Submitting attachments" => "Mengirim pesan dengan lampiran",
        "Delivery unconfirmed · Enter on an empty draft to check and retry" => {
            "Pengiriman belum pasti · Enter pada draf kosong untuk memeriksa dan mencoba lagi"
        }
        "Delivery unconfirmed: {reason}" => "Pengiriman belum dikonfirmasi: {reason}",
        "Delivery unconfirmed: {reason}. Enter to check and retry, Ctrl+C to exit" => {
            "Pengiriman belum dikonfirmasi: {reason}. Enter untuk memeriksa dan mencoba lagi, Ctrl+C untuk keluar"
        }
        "The selected device restarted" => "Perangkat yang dipilih memulai ulang",
        "The original submission was accepted" => "Pengiriman awal telah diterima",
        "The current model does not accept clipboard images; draft retained" => {
            "Model saat ini tidak menerima gambar papan klip. Draf disimpan"
        }
        "Details unavailable; collapse and expand to retry" => {
            "Detail tidak tersedia; tutup lalu buka untuk mencoba lagi"
        }
        "The response above is incomplete." => "Respons di atas tidak lengkap.",
        "Could not reach the provider, or the connection dropped" => {
            "Tidak dapat menjangkau provider, atau koneksi terputus"
        }
        "The provider is rate limiting requests" => "Provider sedang membatasi permintaan",
        "The provider reported a server error" => "Provider melaporkan kesalahan server",
        "The provider sent a response Dim could not use" => {
            "Provider mengirim respons yang tidak dapat digunakan Dim"
        }
        "The provider rejected the credentials" => "Provider menolak kredensial",
        "The account has no quota left" => "Akun tidak memiliki kuota tersisa",
        "The provider did not find the model or endpoint" => {
            "Provider tidak menemukan model atau endpoint"
        }
        "The conversation is longer than the model accepts" => {
            "Percakapan lebih panjang dari yang diterima model"
        }
        "The provider's content filter blocked the response" => {
            "Filter konten provider memblokir respons"
        }
        "The provider rejected the request" => "Provider menolak permintaan",
        "The model stopped at its output limit" => "Model berhenti pada batas outputnya",
        "The model request failed" => "Permintaan model gagal",
        "Check the network or proxy." => "Periksa jaringan atau proxy.",
        "Wait and try again." => "Tunggu dan coba lagi.",
        "Try again later or choose another model." => "Coba lagi nanti atau pilih model lain.",
        "Try again; if it repeats, choose another model or check the proxy or gateway." => {
            "Coba lagi; jika berulang, pilih model lain atau periksa proxy atau gateway."
        }
        "Sign in again or check the API key." => "Masuk lagi atau periksa API key.",
        "Check the plan or billing." => "Periksa paket atau tagihan.",
        "Check the model and the provider's base URL." => "Periksa model dan base URL provider.",
        "Compact the conversation or start a new session." => {
            "Ringkas percakapan atau mulai sesi baru."
        }
        "Rephrase the request." => "Tulis ulang permintaan.",
        "See the details." => "Lihat detail.",
        "Increase the output token limit and retry." => {
            "Naikkan batas token output lalu coba lagi."
        }
        "retrying {attempt}/{max} in {seconds} s: {reason}" => {
            "Mencoba lagi {attempt}/{max} dalam {seconds} s: {reason}"
        }
        "Retrying {attempt}/{max} in {seconds} s" => {
            "Mencoba lagi {attempt}/{max} dalam {seconds} s"
        }
        "Retrying {attempt}/{max} in {seconds} s · {reason} · Esc stops" => {
            "Mencoba lagi {attempt}/{max} dalam {seconds} s · {reason} · Esc berhenti"
        }
        "kind: {kind}" => "kind: {kind}",
        "status: {status}" => "status: {status}",
        "provider code: {code}" => "kode provider: {code}",
        "request id: {id}" => "id permintaan: {id}",
        "tried {count} times over {seconds} s" => "{count} percobaan dalam {seconds} s",
        "the provider asked to wait {seconds} s" => "provider meminta menunggu {seconds} s",
        "cause:" => "penyebab:",
        "Tried {count} times over {seconds} s." => "{count} percobaan dalam {seconds} s.",
        "Waiting for the provider" => "Menunggu provider",
        "Waiting for the provider · {reason} · Esc stops" => {
            "Menunggu provider · {reason} · Esc berhenti"
        }
        _ => return None,
    })
}
