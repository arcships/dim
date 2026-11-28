//! The `compaction` settings document's contract (#2235 PR 1b): the value
//! grammar the write boundary's schema and the parser share, the document's
//! serialization round-trip, the exact/bare/default selection order, and the
//! shaping both the server and the client compute with the same functions.

use dim_agent_types::compaction_settings::{
    CompactionSettings, CompactionWindowSpec, ShapedWindow, shape_compaction_window,
};

#[test]
fn compaction_window_specs_parse_and_roundtrip() {
    assert_eq!(
        CompactionWindowSpec::parse("auto"),
        Ok(CompactionWindowSpec::Auto)
    );
    assert_eq!(
        CompactionWindowSpec::parse("400k"),
        Ok(CompactionWindowSpec::Tokens(400_000))
    );
    assert_eq!(
        CompactionWindowSpec::parse("1M"),
        Ok(CompactionWindowSpec::Tokens(1_000_000))
    );
    assert_eq!(
        CompactionWindowSpec::parse("128000"),
        Ok(CompactionWindowSpec::Tokens(128_000))
    );
    assert_eq!(
        CompactionWindowSpec::parse("40%"),
        Ok(CompactionWindowSpec::Percent(40.0))
    );
    assert_eq!(
        CompactionWindowSpec::parse("0.5%"),
        Ok(CompactionWindowSpec::Percent(0.5))
    );
    assert_eq!(
        CompactionWindowSpec::parse("100%"),
        Ok(CompactionWindowSpec::Percent(100.0))
    );
    assert_eq!(
        CompactionWindowSpec::parse("100.0%"),
        Ok(CompactionWindowSpec::Percent(100.0))
    );
    assert_eq!(
        CompactionWindowSpec::parse("99.5%"),
        Ok(CompactionWindowSpec::Percent(99.5))
    );
    assert_eq!(
        CompactionWindowSpec::parse("0.05%"),
        Ok(CompactionWindowSpec::Percent(0.05))
    );
    assert_eq!(
        CompactionWindowSpec::parse("999999999999999"),
        Ok(CompactionWindowSpec::Tokens(999_999_999_999_999))
    );
    assert_eq!(
        CompactionWindowSpec::parse("900719925474k"),
        Ok(CompactionWindowSpec::Tokens(900_719_925_474_000))
    );
    assert_eq!(
        CompactionWindowSpec::parse("999999999m"),
        Ok(CompactionWindowSpec::Tokens(999_999_999_000_000))
    );
    // The grammar is the write boundary schema's one: neither side of the
    // wire accepts a value the other refuses.
    for invalid in [
        "",
        "AUTO",
        "0",
        "0k",
        "00",
        "0%",
        "0.0%",
        "-5%",
        "+5%",
        ".5%",
        "05%",
        "100.5%",
        "101%",
        "abc",
        "1.5m",
        "k",
        "%",
        "0400k",
        "0400",
        "9007199254740992",
        "9007199254741k",
        "9007199254740k",
        "1000000000000000",
        "9999999999m",
        "400kk",
    ] {
        assert!(
            CompactionWindowSpec::parse(invalid).is_err(),
            "{invalid} must not parse"
        );
    }
    let document: CompactionSettings = serde_json::from_str(
        r#"{"default":"40%","models":{"p/m":"400k","m":400000,"other/auto":"auto"}}"#,
    )
    .unwrap();
    assert_eq!(document.default, CompactionWindowSpec::Percent(40.0));
    assert_eq!(
        document.models["p/m"],
        CompactionWindowSpec::Tokens(400_000)
    );
    assert_eq!(document.models["m"], CompactionWindowSpec::Tokens(400_000));
    assert_eq!(document.models["other/auto"], CompactionWindowSpec::Auto);
    let serialized = serde_json::to_value(&document).unwrap();
    assert_eq!(serialized["default"], serde_json::json!("40%"));
    assert_eq!(serialized["models"]["p/m"], serde_json::json!(400000));
    assert_eq!(serialized["models"]["m"], serde_json::json!(400000));
    assert_eq!(
        serialized["models"]["other/auto"],
        serde_json::json!("auto")
    );
    assert_eq!(CompactionWindowSpec::Auto.display(), "auto");
}

#[test]
fn spec_selection_prefers_exact_then_bare_then_default() {
    let document: CompactionSettings =
        serde_json::from_str(r#"{"default":"40%","models":{"p/m":"400k","m":"60%"}}"#).unwrap();
    assert_eq!(
        document.spec_for("p", "m"),
        CompactionWindowSpec::Tokens(400_000)
    );
    assert_eq!(
        document.spec_for("other", "m"),
        CompactionWindowSpec::Percent(60.0)
    );
    assert_eq!(
        document.spec_for("other", "missing"),
        CompactionWindowSpec::Percent(40.0)
    );
    let automatic: CompactionSettings =
        serde_json::from_str(r#"{"default":"400k","models":{"p/m":"auto"}}"#).unwrap();
    assert_eq!(
        automatic.spec_for("p", "m"),
        CompactionWindowSpec::Auto,
        "an explicit auto overrides a non-auto default"
    );
}

#[test]
fn shaping_soft_caps_raises_and_leaves_auto_alone() {
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Auto, 128_000),
        ShapedWindow::ModelDerived
    );
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Tokens(200_000), 128_000),
        ShapedWindow::ModelDerived,
        "a token value wider than the window is a soft cap"
    );
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Tokens(64_000), 128_000),
        ShapedWindow::Shaped {
            tokens: 64_000,
            raised_from: None
        }
    );
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Tokens(4_000), 128_000),
        ShapedWindow::Shaped {
            tokens: 12_800,
            raised_from: Some(4_000)
        },
        "the floor lifts a small explicit value to a tenth of the window"
    );
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Tokens(4_000), 1_000_000),
        ShapedWindow::Shaped {
            tokens: 32_000,
            raised_from: Some(4_000)
        },
        "the floor is 32k on wide windows"
    );
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Percent(50.0), 128_000),
        ShapedWindow::Shaped {
            tokens: 64_000,
            raised_from: None
        }
    );
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Percent(100.0), 128_000),
        ShapedWindow::Shaped {
            tokens: 128_000,
            raised_from: None
        }
    );
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Percent(3.0), 128_000),
        ShapedWindow::Shaped {
            tokens: 12_800,
            raised_from: Some(3_840)
        }
    );
    assert_eq!(
        shape_compaction_window(CompactionWindowSpec::Tokens(128_000), 128_000),
        ShapedWindow::Shaped {
            tokens: 128_000,
            raised_from: None
        },
        "a token value at the window exactly is not capped"
    );
}
