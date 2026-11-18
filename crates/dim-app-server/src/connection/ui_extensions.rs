use super::*;
use crate::ui_extensions::UiExtensions;
use rutis::host::FiberStatusChanged;
use tokio::sync::watch;

impl Connection {
    pub(crate) fn follow_ui_extensions(self: &Arc<Self>, ctx: &Ctx) -> rutis::Result<()> {
        let (changed, mut availability) = watch::channel(());
        let listener = ctx.on::<FiberStatusChanged>(move |_event, _call| {
            changed.send_replace(());
            async { Ok(None) }
        })?;
        let mut owner = ctx.get::<UiExtensions>();
        let mut revisions = owner.as_ref().map(|owner| owner.subscribe());
        let connection = self.clone();
        let ctx = ctx.clone();
        let mut last = revisions
            .as_mut()
            .map(|revisions| *revisions.borrow_and_update())
            .unwrap_or(0);
        self.hub.tasks.spawn(async move {
            loop {
                tokio::select! {
                    _ = connection.closed.cancelled() => break,
                    result = availability.changed() => if result.is_err() { break; },
                    _ = async {
                        match &mut revisions {
                            Some(revisions) => {
                                let _ = revisions.changed().await;
                            }
                            None => std::future::pending().await,
                        }
                    } => {},
                }
                let current = ctx.get::<UiExtensions>();
                let replaced = match (&owner, &current) {
                    (Some(old), Some(new)) => !Arc::ptr_eq(old, new),
                    (None, None) => false,
                    _ => true,
                };
                if replaced {
                    revisions = current.as_ref().map(|owner| owner.subscribe());
                    owner = current;
                }
                let revision = match &mut revisions {
                    Some(revisions) => *revisions.borrow_and_update(),
                    None => 0,
                };
                if !replaced && revision == last {
                    continue;
                }
                last = revision;
                let send = connection.sender.send(
                    Outbound::notice(Notification::UiExtensionsChanged { revision }),
                    false,
                );
                if send.is_err() {
                    break;
                }
            }
            let _ = listener.dispose().await;
        });
        Ok(())
    }
}
