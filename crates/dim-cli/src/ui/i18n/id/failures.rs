//! Indonesian model failures, retries and their details.
pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
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
        "Waiting for the provider · {reason} · Esc stops" => {
            "Menunggu provider · {reason} · Esc berhenti"
        }
        _ => return None,
    })
}
