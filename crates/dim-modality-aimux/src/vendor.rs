//! The binding between a catalog provider and the aimux provider that
//! executes its media models.
//!
//! One row per bundled catalog provider that declares a media operation and
//! that an aimux vendor package serves. The catalog id is the provider record
//! a seeded builtin keeps; a created provider's id is a UUID, so it can never
//! match a row here and its media operations report `adapter` instead.
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

    /// The base URL of one call: the catalog's value, held to the package's
    /// own path shape.
    pub fn base_url(&self, operation: Operation, url: &str) -> String {
        let url = url.trim_end_matches('/');
        match (self.base, operation) {
            (Base::FalImage, Operation::Image) => {
                if let Some(rest) = url.strip_prefix("https://queue.fal.run") {
                    format!("https://fal.run{rest}")
                } else {
                    url.to_owned()
                }
            }
            (Base::Strip(suffix), _) => url
                .strip_suffix(suffix)
                .map(str::to_owned)
                .unwrap_or_else(|| url.to_owned()),
            (Base::Append(suffix), _) => {
                if url.ends_with(suffix) {
                    url.to_owned()
                } else {
                    format!("{url}{suffix}")
                }
            }
            (_, _) => url.to_owned(),
        }
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
/// `Provider` implementation of each package under `vendor/aimux/aimux-providers`.
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
