//! Binds the desktop tools lane ([`crate::desktop_tools`]) to the bridge's
//! [`ToolsHook`]. Tools get a [`HostedSession`] only when the bound session
//! is exactly the URL and bearer of this app's own running local server.

use crate::desktop::local::{BoxFut, SessionCtx, ToolsHook};
use crate::desktop_tools::{DesktopTools, HostedSession};

fn hosted(s: &SessionCtx) -> Option<HostedSession> {
    HostedSession::verify(
        s.local_url.as_deref(),
        s.local_token.as_ref(),
        &s.session_url,
        &s.session_token,
        &s.account_id,
    )
}

impl ToolsHook for DesktopTools {
    fn handle<'a>(
        &'a self,
        method: &'a str,
        path: &'a str,
        body: &'a [u8],
        session: Option<&'a SessionCtx>,
    ) -> BoxFut<'a, (u16, serde_json::Value)> {
        Box::pin(async move {
            let h = session.and_then(hosted);
            DesktopTools::handle(self, method, path, body, h).await
        })
    }

    fn session_bound<'a>(&'a self, session: &'a SessionCtx) -> BoxFut<'a, ()> {
        Box::pin(async move {
            match hosted(session) {
                Some(h) => self.resume(h).await,
                None => DesktopTools::stop_all(self).await,
            }
        })
    }

    fn stop_all(&self) -> BoxFut<'_, ()> {
        Box::pin(DesktopTools::stop_all(self))
    }
}
