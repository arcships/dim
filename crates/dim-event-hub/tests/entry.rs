//! The Session guide entry is the design's A.6 code block, byte for byte.
#[test]
fn the_entry_is_the_design_code_block() {
    let design = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/design/plugins/EventHub.md"
    ))
    .unwrap();
    let section = design.split("### A.6").nth(1).expect("the design has A.6");
    let block = section
        .split("```text\n")
        .nth(1)
        .expect("A.6 has the entry's code block")
        .split("\n```")
        .next()
        .unwrap();
    assert_eq!(dim_event_hub::ENTRY, block);
    assert_eq!(dim_event_hub::ENTRY.len(), 953);
}
