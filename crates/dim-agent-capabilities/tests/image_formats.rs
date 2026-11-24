//! The destination format rule a request, a standalone call and the provider
//! encoder share: each protocol takes only the image formats its documents
//! list, the vendor package a compatible destination names can narrow the
//! protocol's four, and a destination whose protocol is unknown takes the four
//! a request may carry.
use dim_agent_capabilities::images::takes_image_format;
use dim_agent_types::ModelProtocol;

const REQUEST_FORMATS: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// One destination: the formats it takes and the ones it must refuse.
struct Destination {
    protocol: Option<ModelProtocol>,
    preset: Option<&'static str>,
    takes: &'static [&'static str],
    refuses: &'static [&'static str],
}

#[test]
fn a_destination_takes_only_the_image_formats_its_protocol_documents() {
    let four = &REQUEST_FORMATS[..];
    let destinations = [
        Destination {
            protocol: None,
            preset: None,
            takes: four,
            refuses: &["image/heic", "image/svg+xml"],
        },
        Destination {
            protocol: Some(ModelProtocol::Anthropic),
            preset: None,
            takes: four,
            refuses: &["image/heic", "image/svg+xml"],
        },
        Destination {
            protocol: Some(ModelProtocol::OpenaiChat),
            preset: None,
            takes: four,
            refuses: &["image/heic", "image/svg+xml"],
        },
        Destination {
            protocol: Some(ModelProtocol::OpenaiResponses),
            preset: None,
            takes: four,
            refuses: &["image/heic", "image/svg+xml"],
        },
        Destination {
            protocol: Some(ModelProtocol::OpenaiCompatible),
            preset: None,
            takes: four,
            refuses: &["image/heic", "image/svg+xml"],
        },
        Destination {
            protocol: Some(ModelProtocol::OpenaiCompatible),
            preset: Some("deepseek"),
            takes: four,
            refuses: &["image/heic", "image/svg+xml"],
        },
        Destination {
            protocol: Some(ModelProtocol::OpenaiCompatible),
            preset: Some("xai"),
            takes: &["image/png", "image/jpeg"],
            refuses: &["image/webp", "image/gif", "image/heic"],
        },
        Destination {
            protocol: Some(ModelProtocol::Bedrock),
            preset: None,
            takes: four,
            refuses: &["image/heic", "image/svg+xml"],
        },
        Destination {
            protocol: Some(ModelProtocol::Gemini),
            preset: None,
            takes: &[
                "image/png",
                "image/jpeg",
                "image/webp",
                "image/heic",
                "image/heif",
            ],
            refuses: &["image/gif", "image/svg+xml"],
        },
        Destination {
            protocol: Some(ModelProtocol::Xai),
            preset: None,
            takes: &["image/png", "image/jpeg"],
            refuses: &["image/webp", "image/gif", "image/heic"],
        },
        Destination {
            protocol: Some(ModelProtocol::XaiResponses),
            preset: None,
            takes: &["image/png", "image/jpeg"],
            refuses: &["image/webp", "image/gif", "image/heic"],
        },
    ];
    for destination in destinations {
        for media_type in destination.takes {
            assert!(
                takes_image_format(destination.protocol, destination.preset, media_type),
                "{:?}/{:?} takes {media_type}",
                destination.protocol,
                destination.preset
            );
        }
        for media_type in destination.refuses {
            assert!(
                !takes_image_format(destination.protocol, destination.preset, media_type),
                "{:?}/{:?} refuses {media_type}",
                destination.protocol,
                destination.preset
            );
        }
    }
}
