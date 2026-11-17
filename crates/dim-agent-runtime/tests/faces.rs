use dim_agent_runtime::{AgentBuilder, AgentConfig, Names, PartRegistry};
use rutis_bridge::session::HostDispatch;

struct Demo;

#[test]
fn internal_reasons_are_recorded_by_catalog_name() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.internal_name("secretHost", "secret");
    builder.internal::<Demo>("owner-api");

    assert_eq!(builder.faces().reason("secretHost"), Some("secret"));
    assert_eq!(builder.faces().reason("demo"), Some("owner-api"));
    assert_eq!(builder.faces().reason("dataHost"), None);
    assert_eq!(builder.faces().iter().count(), 2);
}

#[test]
fn a_leaf_cannot_key_an_internal_name() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.internal_name("partRegistry", "owner-api");
    let catalog = dim_agent_runtime::names::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy_with_faces(&catalog, &build, builder.faces());
    let error = names.key::<PartRegistry>().unwrap_err();
    assert!(error.to_string().contains("internal_service"));
    assert!(names.named::<dyn HostDispatch>("partRegistry").is_err());
}

#[test]
#[should_panic(expected = "unknown internal reason")]
fn an_unknown_reason_is_rejected_at_registration() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.internal_name("x", "hotpath");
}
