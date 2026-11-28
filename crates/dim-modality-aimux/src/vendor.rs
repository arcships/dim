//! The binding between a provider and the aimux package that executes its
//! media models.
//!
//! One row per bundled catalog provider that declares a media operation and
//! that an aimux vendor package serves, resolved by the record's own id
//! first. A provider whose id names no row falls back to the family only
//! when the whitelist admits it (Modality A.2): a created provider's
//! canonical lowercase UUID, the Dim account relay, and the two aggregator
//! gateways this build carries contract tests for (`aihubmix`, `zenmux`).
//! Every other rowless bundled id reports `adapter`: the vendor documented
//! neither the endpoints nor the response shape the compatible package can
//! consume. The whitelist itself resolves by the protocol its media model
//! routes to (Modality A.2): the OpenAI-compatible family serves video
//! generation through the openai-compatible package and image generation and
//! edit, and speech and transcription, through the OpenAI one. A model that
//! routes to another protocol family, or the family a row does not serve,
//! reports `adapter`.
//!
//! `kinds` is read from each package's `Provider` implementation, not from its
//! files: xAI ships an image model file whose `image_model` answers
//! `NoSuchModel`, and most single-modality packages implement the other three
//! as `None`. `image_edit` records the step past that: whether the image model
//! reads the bytes an edit sends. A package that ignores `options.files`, or
//! takes URL files only, serves generation alone.
//!
//! `base` is the catalog's `defaultBaseUrl` and the paths the package appends.
//! Three rows compose wrongly with the catalog and are adjusted here, the way
//! the chat driver adjusts its own routes; the adjustment changes nothing when
//! a user has replaced the base URL with their own.

/// One media operation an adapter row serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Image,
    Video,
    Speech,
    Transcription,
}

/// The media kinds one aimux package serves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Kinds {
    pub image: bool,
    /// Whether the image package reads `options.files` with the bytes an edit
    /// sends. A package that ignores them, or takes URL files only, serves
    /// `image.generate` alone.
    pub image_edit: bool,
    pub video: bool,
    pub speech: bool,
    pub transcription: bool,
}

impl Kinds {
    /// Whether the row serves `operation`.
    pub fn serves(self, operation: Operation) -> bool {
        match operation {
            Operation::Image => self.image,
            Operation::Video => self.video,
            Operation::Speech => self.speech,
            Operation::Transcription => self.transcription,
        }
    }
}

/// How a row composes the catalog's base URL with the package's paths.
#[derive(Clone, Copy, Debug)]
pub enum Base {
    /// The package appends its paths to the catalog's base URL as it is.
    AsIs,
    /// fal's image model reads the API host, not the queue host the catalog
    /// carries for the same provider's video and transcription models.
    FalImage,
    /// The catalog's base URL already ends with the segment the package
    /// appends.
    Strip(&'static str),
    /// The package expects a segment the catalog's base URL lacks.
    Append(&'static str),
}

/// One row: a catalog provider id, the aimux provider that executes it, the
/// media kinds it serves and how its base URL composes.
#[derive(Debug)]
pub struct Vendor {
    pub catalog: &'static str,
    pub aimux: &'static str,
    /// The `providerOptions` key the package reads its own options from. The
    /// package names it, not the catalog: Black Forest Labs reads
    /// `blackForestLabs` whatever its provider is called.
    pub options: &'static str,
    pub kinds: Kinds,
    pub base: Base,
    /// The `providerOptions` key the package reads an image output format
    /// from, when it reads one and spells it differently from aimux's
    /// `outputFormat`. A package that forwards its options verbatim reads
    /// the wire spelling; a package that parses them reads the camelCase it
    /// maps onto the wire itself. The requested format is the naming
    /// fallback for a provider that returns bytes with no signature, so a
    /// row without one still names its files.
    pub format: Option<&'static str>,
}

impl Vendor {
    /// The A.1 `<category>.<operation>` statements this row serves, in order.
    ///
    /// `edit` is the model's answer from
    /// [`crate::media::serves_edit`]: the row says whether its package reads
    /// an edit's input files at all, and the package's own
    /// `supports_file_inputs` narrows that per model.
    pub fn statements(&self, edit: bool) -> Vec<(Operation, &'static str, &'static str)> {
        let mut statements = Vec::with_capacity(5);
        if self.kinds.image {
            statements.push((Operation::Image, "image", "generate"));
            if edit {
                statements.push((Operation::Image, "image", "edit"));
            }
        }
        if self.kinds.video {
            statements.push((Operation::Video, "video", "generate"));
        }
        if self.kinds.speech {
            statements.push((Operation::Speech, "speech", "synthesize"));
        }
        if self.kinds.transcription {
            statements.push((Operation::Transcription, "speech", "transcribe"));
        }
        statements
    }
}

/// The kinds of a package whose image model reads an edit's input files.
const fn kinds(image: bool, video: bool, speech: bool, transcription: bool) -> Kinds {
    Kinds {
        image,
        image_edit: image,
        video,
        speech,
        transcription,
    }
}

/// The kinds of a package whose image model ignores an edit's input files:
/// it generates whatever the prompt says and drops the source image.
const fn generate_only(image: bool, video: bool, speech: bool, transcription: bool) -> Kinds {
    Kinds {
        image,
        image_edit: false,
        video,
        speech,
        transcription,
    }
}

/// Every bundled catalog provider whose media models an aimux package serves.
///
/// Built from `crates/dim-app-server/resources/default-catalog.json` and the
/// `Provider` implementation of each package in `aimux-providers`.
pub static VENDORS: &[Vendor] = &[
    // OpenAI's own paths are `/images/generations`, `/images/edits`,
    // `/audio/speech` and `/audio/transcriptions` on the catalog's `/v1` base.
    // The edit path reads `ImageFile::File` bytes (`openai/image/image_model.rs`
    // `:290-295`), so the row serves edit too; a model the package reports as
    // taking no file input drops it. Dall-e-3 is the one such model aimux
    // names.
    Vendor {
        catalog: "openai",
        aimux: "openai",
        options: "openai",
        kinds: kinds(true, false, true, true),
        base: Base::AsIs,
        format: Some("outputFormat"),
    },
    // Gemini appends `/models/{model}:{method}` (and `/interactions` for
    // transcription); the catalog carries no base URL, so the provider's own
    // is the user's `https://generativelanguage.googleapis.com/v1beta`.
    // `ImageFile::File` becomes `inlineData` on the same call
    // (`google/image_model.rs:96`), so the row serves edit too.
    Vendor {
        catalog: "google",
        aimux: "google",
        options: "google",
        kinds: kinds(true, true, true, true),
        base: Base::AsIs,
        format: None,
    },
    // xAI's package refuses image models: the catalog declares Grok Imagine
    // image models this build cannot run, and they report `adapter`.
    Vendor {
        catalog: "xai",
        aimux: "xai",
        options: "xai",
        kinds: kinds(false, true, true, true),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "grok-build",
        aimux: "xai",
        options: "xai",
        kinds: kinds(false, true, true, true),
        base: Base::AsIs,
        format: None,
    },
    // `/v1/text-to-speech/{voice}` and `/v1/speech-to-text` on the API host.
    Vendor {
        catalog: "elevenlabs",
        aimux: "elevenlabs",
        options: "elevenlabs",
        kinds: kinds(false, false, true, true),
        base: Base::AsIs,
        format: None,
    },
    // The image model reads the API host; video and transcription read the
    // queue host the catalog carries. Input files (bytes included) become
    // data URIs on the same model path (`fal.rs:882`), so the row serves edit
    // too.
    Vendor {
        catalog: "fal",
        aimux: "fal",
        options: "fal",
        kinds: kinds(true, true, false, true),
        base: Base::FalImage,
        format: Some("outputFormat"),
    },
    // Deepgram's package serves transcription only; its catalog model for
    // synthesis reports `adapter`.
    Vendor {
        catalog: "deepgram",
        aimux: "deepgram",
        options: "deepgram",
        kinds: kinds(false, false, false, true),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "assemblyai",
        aimux: "assemblyai",
        options: "assemblyai",
        kinds: kinds(false, false, false, true),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "cartesia",
        aimux: "cartesia",
        options: "cartesia",
        kinds: kinds(false, false, true, true),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "gladia",
        aimux: "gladia",
        options: "gladia",
        kinds: kinds(false, false, false, true),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "groq",
        aimux: "groq",
        options: "groq",
        kinds: kinds(false, false, false, true),
        base: Base::AsIs,
        format: None,
    },
    // Input files (bytes included) become data URIs on the prediction path
    // (`replicate.rs:275`), so the image model serves edit too.
    Vendor {
        catalog: "replicate",
        aimux: "replicate",
        options: "replicate",
        kinds: kinds(true, true, false, false),
        base: Base::AsIs,
        format: None,
    },
    // The catalog's `/v2beta` is the segment the package appends. The package
    // posts prompt-only multipart to `/stable-image/generate/{subpath}` and
    // never reads `options.files` (`stability.rs:320-365`), so it serves
    // generate only: an edit would drop the source image.
    Vendor {
        catalog: "stability-ai",
        aimux: "stability",
        options: "stability",
        kinds: generate_only(true, false, false, false),
        base: Base::Strip("/v2beta"),
        format: Some("output_format"),
    },
    // The package appends `/images/generations` to a `/v1` base. It builds
    // its JSON from the prompt and never reads `options.files`
    // (`recraft.rs:348`), so it serves generate only.
    Vendor {
        catalog: "recraft",
        aimux: "recraft",
        options: "recraft",
        kinds: generate_only(true, false, false, false),
        base: Base::Append("/v1"),
        format: None,
    },
    // `/{model_id}` on the catalog's `/v1`. Input files (bytes included)
    // become base64 input images on the same path
    // (`black_forest_labs.rs:280`), so the row serves edit too.
    Vendor {
        catalog: "black-forest-labs",
        aimux: "black_forest_labs",
        options: "blackForestLabs",
        kinds: kinds(true, false, false, false),
        base: Base::AsIs,
        format: Some("outputFormat"),
    },
    // The package takes URL files only, and only for `photon-1` and
    // `photon-flash-1` (`luma.rs:238-241`, `:298-306`); a task sends bytes, so
    // the row serves generate only.
    Vendor {
        catalog: "luma-ai",
        aimux: "luma",
        options: "luma",
        kinds: generate_only(true, false, false, false),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "runway",
        aimux: "runwayml",
        options: "runwayml",
        kinds: kinds(false, true, false, false),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "kling",
        aimux: "klingai",
        options: "klingai",
        kinds: kinds(false, true, false, false),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "hume-ai",
        aimux: "hume",
        options: "hume",
        kinds: kinds(false, false, true, false),
        base: Base::AsIs,
        format: None,
    },
    Vendor {
        catalog: "mistral-ai",
        aimux: "mistral",
        options: "mistral",
        kinds: kinds(false, false, true, true),
        base: Base::AsIs,
        format: None,
    },
    // The catalog's `/speechtotext/v1` is the prefix the package appends.
    Vendor {
        catalog: "rev-ai",
        aimux: "revai",
        options: "revai",
        kinds: kinds(false, false, false, true),
        base: Base::Strip("/speechtotext/v1"),
        format: None,
    },
];

/// The row for a provider record, if this build serves its media models.
pub fn vendor(provider_id: &str) -> Option<&'static Vendor> {
    VENDORS.iter().find(|vendor| vendor.catalog == provider_id)
}

/// Whether a rowless provider may use the compatible family fallback
/// (Modality A.2): a created provider's canonical lowercase UUID, the Dim
/// account relay, and the two aggregator gateways this build carries
/// contract tests for. Every other rowless bundled id is refused: the
/// vendors behind them document neither the endpoints nor the response
/// shapes the compatible packages can consume, so listing them available
/// would send requests whose results cannot be read.
fn eligible_for_fallback(provider_id: &str) -> bool {
    uuid::Uuid::parse_str(provider_id).is_ok_and(|id| id.to_string() == provider_id)
        || provider_id == "dimcode-api-oauth"
        || provider_id == "aihubmix"
        || provider_id == "zenmux"
}

use dim_agent_types::in_word_family;

/// Whether the model id names the Ark video word family. The Ark video API
/// is a different route from the compatible family's `/videos`
/// (`/contents/generations/tasks`, JSON), so the compatible binding cannot
/// execute these models; they are refused until the per-model route exists.
fn is_ark_video_model(model_id: &str) -> bool {
    in_word_family(model_id, "seedance")
}

/// Whether the model id names the Seedream image word family (`seedream`,
/// `doubao-seedream`). A Seedream endpoint answers image URLs by default, and
/// the compatible package reads inline bytes alone, so the compatible
/// binding cannot execute these models; they are refused until the per-model
/// route exists.
fn is_seedream_image_model(model_id: &str) -> bool {
    in_word_family(model_id, "seedream")
}

/// The catalog image entries whose endpoint documentation names the inline
/// response shape the compatible package reads. aihubmix publishes a
/// per-model endpoint schema (`/call/schema/models/{id}/endpoints`, no
/// auth): these entries' `/v1/images/generations` schema lists
/// `response_format` with the inline enum value, so the binding's default
/// request reads bytes back. Entries absent from this table — a schema that
/// lists `url` alone, no `/v1/images/generations` endpoint at all, or a
/// schema the gateway does not publish — document no way to read the bytes,
/// so the generic fallback refuses them rather than billing a generation
/// whose answer never reads back.
const AIHUBMIX_INLINE_IMAGE_MODELS: &[&str] = &[
    "agnes-image-2.1-flash",
    "doubao-seedream-4-0",
    "doubao-seedream-4-5",
    "doubao-seedream-5-0-flash",
    "doubao-seedream-5-0-lite",
    "ernie-image-turbo",
    "flux-2-flex",
    "flux-2-pro",
    "gemini-2.5-flash-image",
    "gemini-2.5-flash-image-preview",
    "gemini-3.1-flash-image-preview",
    "gpt-image-1",
    "mai-image-2.5",
    "mai-image-2.5-flash",
    "mai-image-2.5-pro",
    "mai-image-2.6",
    "mai-image-2.6-flash",
    "musesteamer-air-image",
    "qwen-image",
    "qwen-image-2.0",
    "qwen-image-2.0-pro",
    "qwen-image-2.1-pro",
    "qwen-image-2.1-turbo",
    "qwen-image-edit",
    "qwen-image-max",
    "wan2.6-t2i",
    "wan2.7-image",
    "wan2.7-image-pro",
];

/// Whether a cataloged entry on one of the two whitelisted aggregator
/// gateways documents the inline response shape. An id starting with the
/// `gpt-image-` or `chatgpt-image-` prefixes after the vendor namespace
/// answers inline bytes without the field — the OpenAI Image API documents
/// base64 for those models, and both gateways pass it through — so those
/// entries need no `response_format` at all. Every other entry must be one
/// of the schema-verified ids above; zenmux publishes no per-model image
/// schema, so nothing beyond the prefixes verifies there.
fn catalog_image_verified(provider_id: &str, model_id: &str) -> bool {
    // Three aihubmix ids answer `model_not_found` in the gateway's own
    // endpoint schema service: the entry is stale, and a stale entry reads
    // nothing whatever its family documents.
    const AIHUBMIX_NOT_FOUND: &[&str] = &["dall-e-3", "gpt-image-1-mini", "gpt-image-test"];
    let tail = model_id.rsplit('/').next().unwrap_or(model_id);
    if provider_id == "aihubmix" && AIHUBMIX_NOT_FOUND.contains(&tail) {
        return false;
    }
    if tail.starts_with("gpt-image-") || tail.starts_with("chatgpt-image-") {
        return true;
    }
    provider_id == "aihubmix" && AIHUBMIX_INLINE_IMAGE_MODELS.contains(&tail)
}

/// Whether the model id answers inline bytes without being asked. aimux's
/// openai package omits `response_format` for these prefixes
/// (aimux-providers 0.7.0 `openai/image/image_model_options.rs`), and the
/// compatible fallback reads the same rule because the gateways behind it
/// follow the OpenAI shape.
pub(crate) fn answers_inline_bytes_by_default(model_id: &str) -> bool {
    let tail = model_id.rsplit('/').next().unwrap_or(model_id);
    tail.starts_with("gpt-image-") || tail.starts_with("chatgpt-image-")
}

/// How a binding builds its aimux provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Package {
    /// The preset of [`aimux_providers::create_provider`], built with
    /// [`aimux_providers::PresetSettings`].
    Preset(&'static str),
    /// [`aimux_providers::create_openai_compatible`], built with
    /// [`aimux_providers::OpenAICompatibleProviderSettings`].
    OpenaiCompatible,
}

/// What one (provider, operation) pair resolves to, vendor row or protocol
/// family alike: the package, its `providerOptions` namespace and base
/// composition, whether its image model reads an edit's input files, and
/// whether an image create never retries.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub package: Package,
    /// The `providerOptions` key the package reads its own options from. The
    /// package names it, not the catalog: Black Forest Labs reads
    /// `blackForestLabs` whatever its provider is called.
    pub options: &'static str,
    pub base: Base,
    /// Whether the image model reads the bytes an edit sends. The
    /// openai-compatible package reads `options.files` on every model, so
    /// the family's image binding keeps edit; [`crate::media::serves_edit`]
    /// narrows it per model where the package reports one.
    pub image_edit: bool,
    /// The `providerOptions` key the package reads an image output format
    /// from, when it reads one and spells it differently from aimux's
    /// `outputFormat`. A package that forwards its options verbatim reads
    /// the wire spelling; a package that parses them reads the camelCase it
    /// maps onto the wire itself. The requested format is the naming
    /// fallback for a provider that returns bytes with no signature, so a
    /// binding without one still names its files.
    pub format: Option<&'static str>,
    /// Whether an image create never retries a timed-out POST. Only the Dim
    /// account relay plays back a request it already accepted, and its media
    /// models resolve to the OpenAI package in both resolutions.
    pub no_retry: bool,
}

impl Vendor {
    /// The binding the row resolves every operation to: one package serves
    /// the whole row, and no vendor row's image create is throttled.
    pub fn binding(&self) -> Binding {
        Binding {
            package: Package::Preset(self.aimux),
            options: self.options,
            base: self.base,
            image_edit: self.kinds.image_edit,
            format: self.format,
            no_retry: false,
        }
    }
}

/// The binding a model's media `operation` resolves to, or `unavailable`
/// when nothing in this build serves it.
///
/// A provider whose id names a vendor row resolves to it whatever the
/// protocol says (Modality A.2): the row is its own package's decision, and
/// xAI's providers route `openai-compatible` while serving their video
/// through the xAI one. A rowless provider falls back to the family only
/// when the whitelist admits it — a created provider's canonical lowercase
/// UUID, the Dim account relay, or one of the two aggregator gateways this
/// build carries contract tests for (`aihubmix`, `zenmux`); every other
/// rowless bundled id reports `adapter`, because the vendors behind them
/// document neither the endpoints nor the response shapes the compatible
/// packages can consume. Only the OpenAI-compatible family carries media at
/// all: video generation and image generation and edit go to the
/// openai-compatible package, and speech and transcription to the OpenAI
/// one, which is the only aimux package that serves them for this family.
/// The relay's image generation is the one exception: its gateway answers
/// the OpenAI Image API itself, so it stays on the OpenAI package with its
/// image create never retried. The compatible image binding asks its gateway
/// for inline bytes by default — the call sends `response_format` through
/// the provider options, and the inline format is the one value a caller's
/// own `response_format` may carry — so a gateway that honors the field
/// answers bytes instead of the default URL shape. On the id prefixes that
/// answer inline bytes without the field, matched after the vendor
/// namespace, nothing is sent at all — not even the caller's own copy of
/// the same inline value — because a strict GPT Image endpoint rejects the
/// field's presence. `None` is startup recovery's
/// answer for a route it cannot re-read: the family answers as the
/// compatible one would, and a route that no longer matches is the call's
/// own failure.
///
/// Three word-family exclusions narrow the fallback further. A video model
/// whose id names the Ark word family (`seedance`, `doubao-seedance`) is
/// refused: the Ark video API is a different route from the compatible
/// family's `/videos`, and the per-model binding that serves it is a later
/// slice (#1927). An image model whose id names the Seedream word family
/// (`seedream`, `doubao-seedream`) is refused by the generic fallback alone —
/// the relay stays on the OpenAI package — because a Seedream endpoint
/// answers image URLs by default while the compatible package reads inline
/// bytes alone, and the per-model binding is the same later slice. The two
/// whitelisted gateways answer one check more: a cataloged image entry is
/// refused unless its endpoint documentation proves the inline shape —
/// the inline-answering id prefixes, or the schema-verified aihubmix ids —
/// because the sweep behind this adapter requires every listed-available
/// image model to read its bytes back (#1927).
pub fn binding(
    provider_id: &str,
    model_id: &str,
    protocol: Option<dim_agent_types::ModelProtocol>,
    operation: Operation,
) -> Result<Binding, dim_modality::AdapterError> {
    let compatible = matches!(
        protocol,
        None | Some(dim_agent_types::ModelProtocol::OpenaiCompatible)
    );
    if let Some(vendor) = vendor(provider_id) {
        return if vendor.kinds.serves(operation) {
            Ok(vendor.binding())
        } else {
            Err(dim_modality::AdapterError::unavailable(format!(
                "The {provider_id} provider serves no {}",
                statement(operation)
            )))
        };
    }
    if !eligible_for_fallback(provider_id) {
        return Err(dim_modality::AdapterError::unavailable(format!(
            "The {provider_id} provider serves no {}",
            statement(operation)
        )));
    }
    if operation == Operation::Video && is_ark_video_model(model_id) {
        return Err(dim_modality::AdapterError::unavailable(format!(
            "The `{model_id}` model's video generation needs its provider's own route"
        )));
    }
    let binding = match (compatible, operation) {
        (true, Operation::Image) if provider_id == "dimcode-api-oauth" => Binding {
            package: Package::Preset("openai"),
            options: "openai",
            base: Base::AsIs,
            image_edit: true,
            format: Some("outputFormat"),
            no_retry: true,
        },
        (true, Operation::Image) if is_seedream_image_model(model_id) => {
            return Err(dim_modality::AdapterError::unavailable(format!(
                "The `{model_id}` model's image generation needs its provider's own route"
            )));
        }
        (true, Operation::Image)
            if (provider_id == "aihubmix" || provider_id == "zenmux")
                && !catalog_image_verified(provider_id, model_id) =>
        {
            return Err(dim_modality::AdapterError::unavailable(format!(
                "The `{model_id}` model's image endpoint does not document the inline response shape this binding reads"
            )));
        }
        (true, Operation::Image) => Binding {
            package: Package::OpenaiCompatible,
            options: "openaiCompatible",
            base: Base::AsIs,
            image_edit: true,
            format: Some("output_format"),
            no_retry: false,
        },
        (true, Operation::Video) => Binding {
            package: Package::OpenaiCompatible,
            options: "openaiCompatible",
            base: Base::AsIs,
            image_edit: false,
            format: None,
            no_retry: false,
        },
        (true, Operation::Speech) | (true, Operation::Transcription) => Binding {
            package: Package::Preset("openai"),
            options: "openai",
            base: Base::AsIs,
            image_edit: false,
            format: None,
            no_retry: false,
        },
        (false, _) => {
            return Err(dim_modality::AdapterError::unavailable(format!(
                "No adapter row serves the provider `{provider_id}`"
            )));
        }
    };
    Ok(binding)
}

/// The `<category>.<operation>` statement an `Operation` names, for a
/// refusal.
fn statement(operation: Operation) -> &'static str {
    match operation {
        Operation::Image => "image",
        Operation::Video => "video.generate",
        Operation::Speech => "speech.synthesize",
        Operation::Transcription => "speech.transcribe",
    }
}
