//! The binding between a provider and the aimux package that executes its
//! media models.
//!
//! One row per bundled catalog provider that declares a media operation and
//! that an aimux vendor package serves, resolved by the record's own id
//! first. A provider whose id names no row — a created provider's UUID, or
//! the Dim account relay — resolves by the protocol its media model routes
//! to (Modality A.2): the OpenAI-compatible family serves video generation
//! through the openai-compatible package and image generation and edit, and
//! speech and transcription, through the OpenAI one. A model that routes to
//! another protocol family, or the family a row does not serve, reports
//! `adapter`.
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
    /// `outputFormat`. The requested format is the naming fallback for a
    /// provider that returns bytes with no signature, so a row without one
    /// still names its files.
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

/// The bundled vendor-role catalog providers whose media models no aimux
/// package serves: they carry the `openai-compatible` placeholder driver
/// kind, so their ids would pass for a created provider's compatible family
/// if the family fallback did not name them. Extracted from
/// `crates/dim-app-server/resources/default-catalog.json`; a new bundled
/// entry of this shape joins the table until its package arrives.
static BUNDLED_WITHOUT_ROWS: &[&str] = &[
    "adobe-firefly",
    "alibaba-dashscope",
    "amazon-speech",
    "beatoven",
    "bytedance",
    "d-id",
    "fish-audio",
    "heygen",
    "ibm-watson-speech",
    "ideogram",
    "mubert",
    "musicapi-ai",
    "pika",
    "pixverse",
    "playht",
    "reve-ai",
    "sarvam-ai",
    "soniox",
    "speechmatics",
    "step-plan",
    "stepfun",
    "suno",
    "synthesia",
    "tavus",
    "vidu",
];

/// Whether the model id names the Ark video word family. The Ark video API
/// is a different route from the compatible family's `/videos`
/// (`/contents/generations/tasks`, JSON), so the compatible binding cannot
/// execute these models; they are refused until the per-model route exists.
fn is_ark_video_model(model_id: &str) -> bool {
    model_id
        .to_ascii_lowercase()
        .split(['.', '/', '_', '-'])
        .any(|part| part == "seedance")
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
    /// `outputFormat`. The requested format is the naming fallback for a
    /// provider that returns bytes with no signature, so a binding without
    /// one still names its files.
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
/// through the xAI one. Only a rowless provider — a created one's UUID, or
/// the Dim account relay — falls back to the family, and only the
/// OpenAI-compatible family carries media at all: video generation and
/// image generation and edit go to the openai-compatible package, and
/// speech and transcription to the OpenAI one, which is the only aimux
/// package that serves them for this family. The relay's image generation
/// is the one exception: its gateway answers the OpenAI Image API, whose
/// package asks for inline bytes the compatible one does not, so it stays
/// on the OpenAI package with its image create never retried. `None` is
/// startup recovery's answer for a route it cannot re-read: the family
/// answers as the compatible one would, and a route that no longer matches
/// is the call's own failure.
///
/// Two exclusions narrow the fallback. A bundled vendor-role provider whose
/// media models no package serves ([`BUNDLED_WITHOUT_ROWS`]) carries the
/// `openai-compatible` placeholder driver kind for its chat models alone:
/// it is not a created provider, and its media models stay unserved rather
/// than answering generic endpoints the vendor never documented. And a
/// video model whose id names the Ark word family (`seedance`,
/// `doubao-seedance`) is refused: the Ark video API is a different route
/// from the compatible family's `/videos`, and the per-model binding that
/// serves it is a later slice (#1927).
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
    if BUNDLED_WITHOUT_ROWS.contains(&provider_id) {
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
        (true, Operation::Image) => Binding {
            package: Package::OpenaiCompatible,
            options: "openaiCompatible",
            base: Base::AsIs,
            image_edit: true,
            format: Some("outputFormat"),
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
