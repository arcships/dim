use crate::catalog_tokens::{split_display, split_id, split_tokens};
use crate::name_inputs::{Name, keys as name_keys};
use flate2::{Compression, write::GzEncoder};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn number(out: &mut Vec<u8>, value: usize) {
    out.extend(u32::try_from(value).unwrap().to_le_bytes());
}

fn span(out: &mut Vec<u8>, data: &mut Vec<u8>, bytes: &[u8]) {
    number(out, data.len());
    number(out, bytes.len());
    data.extend(bytes);
}

fn compressed(value: &Value) -> Vec<u8> {
    let mut gzip = GzEncoder::new(Vec::new(), Compression::best());
    serde_json::to_writer(&mut gzip, value).unwrap();
    gzip.finish().unwrap()
}

pub fn build(out: &Path) {
    let source = "resources/default-catalog.json";
    println!("cargo::rerun-if-changed={source}");
    let bytes = fs::read(source).unwrap();
    let mut catalog: Value = serde_json::from_slice(&bytes).unwrap();
    let providers = catalog["providers"].as_array_mut().unwrap();
    let mut data = vec![0; 32];
    let mut headers = Vec::new();
    let mut rows = Vec::new();
    let mut names = Vec::new();
    let mut keys = Vec::<(String, usize)>::new();
    let mut count = 0;
    for (provider, header) in providers.iter_mut().enumerate() {
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(header).unwrap()));
        let models = header["models"].take();
        header["models"] = serde_json::json!([]);
        let id = header["providerId"].as_str().unwrap();
        keys.push((format!("p\0{id}"), provider));
        let first = count;
        for model in models.as_array().unwrap() {
            let model_id = model["modelId"].as_str().unwrap();
            let name = model["displayName"].as_str().unwrap();
            let label = model
                .pointer("/metadata/raw/magpie_label")
                .and_then(Value::as_str);
            let canonical = model
                .pointer("/metadata/raw/canonical_model_id")
                .and_then(Value::as_str);
            let own_namespace = header
                .pointer("/metadata/vendorNamespace")
                .and_then(Value::as_str);
            let vendor = header.pointer("/metadata/role").and_then(Value::as_str) == Some("vendor")
                && own_namespace.is_none_or(|own| {
                    canonical
                        .and_then(|id| id.split_once('/'))
                        .is_none_or(|(namespace, _)| namespace == own)
                });
            let index = if vendor {
                Some("v")
            } else if id == "custom-provider" {
                Some("c")
            } else {
                None
            };
            let tokens = split_id(model_id);
            keys.push((format!("q\0{}", model_id.to_ascii_lowercase()), count));
            keys.push((format!("t\0{id}\0{model_id}"), count));
            keys.push((format!("m\0{model_id}"), count));
            keys.push((format!("a\0{}", tokens.join("\0")), count));
            if let Some(index) = index {
                keys.push((format!("{index}t\0{}", tokens.join("\0")), count));
                for (kind, tokens) in [
                    ("d", Some(split_display(name))),
                    (
                        "n",
                        canonical
                            .map(|s| split_tokens(s.to_lowercase().rsplit('/').next().unwrap())),
                    ),
                ] {
                    if let Some(tokens) = tokens {
                        for length in 1..=tokens.len() {
                            keys.push((
                                format!("{index}{kind}\0{}", tokens[..length].join("\0")),
                                count,
                            ));
                        }
                    }
                }
            }
            for key in name_keys(&Name {
                model_id: model_id.to_owned(),
                display_name: name.to_owned(),
                label: label.map(str::to_owned),
            }) {
                keys.push((format!("s\0{id}\0{key}"), count));
            }
            number(&mut rows, provider);
            span(&mut rows, &mut data, model_id.as_bytes());
            span(&mut rows, &mut data, tokens.join("\0").as_bytes());
            span(
                &mut rows,
                &mut data,
                model_id
                    .rsplit_once(':')
                    .map(|(_, tag)| tag.to_lowercase())
                    .unwrap_or_default()
                    .as_bytes(),
            );
            span(&mut rows, &mut data, &compressed(model));
            span(&mut rows, &mut data, label.unwrap_or_default().as_bytes());
            span(
                &mut rows,
                &mut data,
                canonical
                    .map(|s| split_tokens(s.to_lowercase().rsplit('/').next().unwrap()).join("\0"))
                    .unwrap_or_default()
                    .as_bytes(),
            );
            span(
                &mut rows,
                &mut data,
                split_display(name).join("\0").as_bytes(),
            );
            span(
                &mut rows,
                &mut data,
                header
                    .pointer("/metadata/raw/endpointTypeByModel")
                    .and_then(|routes| routes.get(model_id))
                    .map(Value::to_string)
                    .unwrap_or_default()
                    .as_bytes(),
            );
            span(&mut names, &mut data, name.as_bytes());
            count += 1;
        }
        span(&mut headers, &mut data, id.as_bytes());
        span(&mut headers, &mut data, fingerprint.as_bytes());
        number(&mut headers, first);
        number(&mut headers, count);
        span(&mut headers, &mut data, &compressed(header));
    }
    keys.sort();
    keys.dedup();
    let mut index = Vec::new();
    for (key, row) in &keys {
        span(&mut index, &mut data, key.as_bytes());
        number(&mut index, *row);
    }
    let mut layout = Vec::new();
    number(&mut layout, data.len());
    number(&mut layout, providers.len());
    data.extend(headers);
    number(&mut layout, data.len());
    number(&mut layout, count);
    data.extend(rows);
    number(&mut layout, data.len());
    number(&mut layout, keys.len());
    data.extend(index);
    number(&mut layout, data.len());
    data.extend(names);
    number(&mut layout, 1);
    data[..32].copy_from_slice(&layout);
    let revision = Sha256::digest(&data);
    fs::write(out.join("catalog.bin"), data).unwrap();
    fs::write(out.join("catalog.rs"), format!("const REVISION: &str = \"{:x}\";\nstatic DATA: CatalogData = CatalogData {{ bytes: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/catalog.bin\")) }};", revision)).unwrap();
}
