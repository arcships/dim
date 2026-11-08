//! The helper's command line: `--input <abs> --artifacts <abs>
//! --output-mode text|layout`. Unknown, duplicate or relative arguments are
//! `invalid_input`; nothing else is accepted.
use crate::contract::{HelperError, OutputMode};
use std::ffi::OsString;
use std::path::PathBuf;

/// The parsed arguments of one helper call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperArguments {
    /// The one input file, absolute.
    pub input: PathBuf,
    /// The directory the artifacts are written into, absolute.
    pub artifacts: PathBuf,
    /// What to extract.
    pub output_mode: OutputMode,
}

impl HelperArguments {
    /// Parses the arguments after the subcommand.
    pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, HelperError> {
        let mut input: Option<PathBuf> = None;
        let mut artifacts: Option<PathBuf> = None;
        let mut output_mode = OutputMode::default();
        let mut output_mode_set = false;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            let flag = argument.to_string_lossy().into_owned();
            match flag.as_str() {
                "--input" => {
                    let value = value(&mut arguments, &flag, input.is_some())?;
                    input = Some(PathBuf::from(value));
                }
                "--artifacts" => {
                    let value = value(&mut arguments, &flag, artifacts.is_some())?;
                    artifacts = Some(PathBuf::from(value));
                }
                "--output-mode" => {
                    let value = value(&mut arguments, &flag, output_mode_set)?;
                    let text = value.to_string_lossy().into_owned();
                    output_mode = OutputMode::parse(&text).ok_or_else(|| {
                        HelperError::invalid_input("Output mode must be text or layout")
                    })?;
                    output_mode_set = true;
                }
                other => {
                    return Err(HelperError::invalid_input(format!(
                        "Unknown argument: {other}"
                    )));
                }
            }
        }
        let Some(input) = input else {
            return Err(HelperError::invalid_input(
                "Usage: ocr-helper --input <path> --artifacts <directory> --output-mode <text|layout>",
            ));
        };
        let Some(artifacts) = artifacts else {
            return Err(HelperError::invalid_input(
                "Usage: ocr-helper --input <path> --artifacts <directory> --output-mode <text|layout>",
            ));
        };
        for path in [&input, &artifacts] {
            if !path.is_absolute() {
                return Err(HelperError::invalid_input(
                    "Input and artifact paths must be absolute",
                ));
            }
        }
        Ok(Self {
            input,
            artifacts,
            output_mode,
        })
    }
}

/// The value of `flag`; a repeated flag or a missing value is `invalid_input`.
fn value(
    arguments: &mut impl Iterator<Item = OsString>,
    flag: &str,
    repeated: bool,
) -> Result<OsString, HelperError> {
    if repeated {
        return Err(HelperError::invalid_input(format!(
            "Duplicate argument: {flag}"
        )));
    }
    let value = arguments
        .next()
        .ok_or_else(|| HelperError::invalid_input(format!("Missing value for {flag}")))?;
    if value.is_empty() || value.to_string_lossy().starts_with("--") {
        return Err(HelperError::invalid_input(format!(
            "Missing value for {flag}"
        )));
    }
    Ok(value)
}
