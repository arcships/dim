//! The `events` tool's description is the design's A.4 code block, byte for
//! byte.
#[test]
fn the_tool_description_is_the_design_code_block() {
    let design = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/design/plugins/EventHub.md"
    ))
    .unwrap();
    let section = design.split("### A.4").nth(1).expect("the design has A.4");
    let section = section.split("### A.5").next().unwrap();
    let block = section
        .split("The description is `EventHubTexts.tool`")
        .nth(1)
        .expect("A.4 says where the description is")
        .split("```text\n")
        .nth(1)
        .expect("A.4 has the description's code block")
        .split("\n```")
        .next()
        .unwrap();
    let texts = dim_event_hub::EventHubTexts::default();
    assert_eq!(texts.tool, block);
    assert!(
        !texts.tool.contains(['\n', '\r']),
        "the description is one line"
    );
}
