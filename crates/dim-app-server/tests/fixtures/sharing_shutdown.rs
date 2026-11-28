use dim_agent_runtime::{AgentBuilder, Feature};
use rutis::{Ctx, Plugin};
use std::time::Duration;

struct DelayedCleanup(bool);

impl Plugin for DelayedCleanup {
    type Deps = ();
    fn name(&self) -> &str {
        "DelayedCleanup"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let fail = self.0;
        ctx.effect(move || async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            if fail {
                return Err(rutis::host::ErrorKind::PluginFailed(
                    std::io::Error::other("fixture cleanup failed").into(),
                )
                .into());
            }
            Ok(())
        })?;
        Ok(())
    }
}

fn install(builder: &mut AgentBuilder) {
    let fail = std::env::var_os("DIM_TEST_FAIL_CLEANUP").is_some();
    builder.require_feature(
        Feature::new("DelayedCleanup").session("DelayedCleanup", move |_| {
            Ok(Box::new(DelayedCleanup(fail)))
        }),
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    dim_app_server::cli::run_with(std::env::args().skip(1), None, install)
}
