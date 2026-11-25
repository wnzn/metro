//! Page limits. They're server-wide settings that org admins change through
//! `/v1/admin/settings` (the web admin panel uses the same API); until a
//! setting is saved, these defaults apply.

use crate::AppState;

/// Default most pages in one Space (and personal pages per person).
pub const DEFAULT_PAGES_PER_SPACE: usize = 50;
/// Default most versions kept per page (version 1 is always kept too).
pub const DEFAULT_VERSIONS_PER_PAGE: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageLimits {
    /// Most live (not deleted) pages per Space; also the most personal
    /// pages per person.
    pub pages_per_space: usize,
    /// Versions kept per page: the newest this many, plus version 1.
    /// Older ones are pruned when a new version is saved, never refused.
    pub versions_per_page: usize,
}

impl Default for PageLimits {
    fn default() -> Self {
        Self {
            pages_per_space: DEFAULT_PAGES_PER_SPACE,
            versions_per_page: DEFAULT_VERSIONS_PER_PAGE,
        }
    }
}

/// Where the page limits come from.
///
/// INTEGRATION POINT (lane `server-ux`): the server-wide settings store
/// behind `GET|PUT /v1/admin/settings` implements this (keys
/// `pages_per_space` and `versions_per_page`) and is installed with
/// [`set_source`] when the state is built. Until then [`Defaults`] applies.
pub trait PageLimitsSource: Send + Sync {
    fn page_limits(&self, db: &crate::db::Db) -> anyhow::Result<PageLimits>;
}

/// The built-in defaults.
pub struct Defaults;

impl PageLimitsSource for Defaults {
    fn page_limits(&self, _db: &crate::db::Db) -> anyhow::Result<PageLimits> {
        Ok(PageLimits::default())
    }
}

static SOURCE: std::sync::OnceLock<Box<dyn PageLimitsSource>> = std::sync::OnceLock::new();

/// Install the settings store's implementation (once per process).
pub fn set_source(source: Box<dyn PageLimitsSource>) -> bool {
    SOURCE.set(source).is_ok()
}

/// The limits in force now. A settings store that fails to answer falls
/// back to the defaults and logs why (limits only bound resources; they
/// never grant access).
pub async fn current(state: &AppState) -> PageLimits {
    let Some(source) = SOURCE.get() else {
        return PageLimits::default();
    };
    let source: &'static dyn PageLimitsSource = source.as_ref();
    match state.db.call(move |db| source.page_limits(db)).await {
        Ok(l) => PageLimits {
            pages_per_space: l.pages_per_space.max(1),
            versions_per_page: l.versions_per_page.max(1),
        },
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "reading page limits; using the defaults");
            PageLimits::default()
        }
    }
}
