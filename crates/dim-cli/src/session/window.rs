use crate::{Result, client::Client, display::Display, wire::HistoryWindow};
use dim_client::window::Window;
use dim_client::wire::DisplayPage;

pub(super) async fn read(
    client: &mut Client,
    session_id: &str,
    display: &mut Display,
) -> Result<u64> {
    if !client.supports(&["session.history.latestTurn"]) {
        return Err(
            "This daemon cannot read a bounded transcript window; update the daemon".into(),
        );
    }
    let mut window = Window::default();
    loop {
        let params = window.params_with_display(
            session_id,
            "main",
            !display.json && client.offers("session.history.display.tail"),
            !display.json && client.offers("session.history.display.transcript"),
        );
        let page: HistoryWindow = if !display.json
            && client.offers("session.history.display")
            && client.offers("session.history.detail")
        {
            client
                .typed::<DisplayPage>("session.history.display", params)
                .await?
                .into()
        } else {
            client.typed("session.history", params).await?
        };
        if display.json {
            page.next(None, None)?;
            return Ok(page.through);
        }
        if window.push(page)? {
            break;
        }
    }
    let events = window.take();
    display.history_before = window.before;
    display.history_through = window.through.unwrap();
    if window.before.is_some() {
        display.record("[earlier content not loaded]")?;
    }
    for event in events {
        display.event(event, true)?;
    }
    Ok(window
        .through
        .expect("the initial window always reads one page"))
}

pub(super) async fn earlier(
    client: &mut Client,
    session_id: &str,
    display: &mut Display,
) -> Result<()> {
    let Some(before) = display.history_before else {
        return display.record("Beginning of conversation");
    };
    let mut window = Window::earlier(before, display.history_through);
    loop {
        let params = window.params_with_display(
            session_id,
            "main",
            !display.json && client.offers("session.history.display.tail"),
            !display.json && client.offers("session.history.display.transcript"),
        );
        let page: HistoryWindow = if !display.json
            && client.offers("session.history.display")
            && client.offers("session.history.detail")
        {
            client
                .typed::<DisplayPage>("session.history.display", params)
                .await?
                .into()
        } else {
            client.typed("session.history", params).await?
        };
        if window.push(page)? {
            break;
        }
    }
    let events = window.take();
    display.history_before = window.before;
    display.record("[earlier conversation]")?;
    let mut history = Display::new(true, false);
    for event in events {
        history.event(event, true)?;
    }
    history.finish_text()?;
    display.record(if window.before.is_some() {
        "[earlier content not loaded; /earlier to continue]"
    } else {
        "[beginning of conversation]"
    })
}
