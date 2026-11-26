//! Personal profiles: a display name, a short bio and an avatar for every
//! signed-in person, whatever their authenticator (dev token, OIDC or a
//! managed account).
//!
//! - The **stable id never changes** and stays the only key for access
//!   control. A profile only changes how a person is shown.
//! - The **display name** is the person's own override if they set one,
//!   otherwise the name their authenticator gives (a managed account's
//!   admin-assigned name, an env user's name, an OIDC display claim). An
//!   override follows the same rules as managed names
//!   ([`crate::admin_users::name_clash`]): never a reserved actor (`agent`,
//!   `runner`, `system`), never id-shaped (`mu:…`, `user:…`), and never
//!   another person's id or name. Setting it to `null` resets it.
//! - New room and Chat events snapshot the name in force when they are
//!   written (history is not rewritten). The model always sees each
//!   author's stable `speaker_id` next to the name.
//! - **Avatars** are uploaded as raw PNG or JPEG bytes (at most
//!   [`MAX_AVATAR_BYTES`], [`MAX_SOURCE_SIDE`] pixels a side), decoded
//!   under allocation limits, downscaled to at most [`AVATAR_SIDE`] pixels
//!   a side and re-encoded as a fresh PNG, so no metadata, colour profile,
//!   animation or trailing bytes survive. Only that PNG is stored and
//!   served, as `image/png`. Nothing is ever fetched from a URL.
//! - **Visibility:** signed-in users can see the cards of people who have
//!   signed in before. Bios are visible to yourself, current Space peers
//!   and DM peers only. There is no admin bypass. Avatars are read with
//!   the reader's bearer token and are never cached (`no-store`).
//!   `avatar_version` changes on every upload so clients can tell when to
//!   fetch again.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Deserializer, Serialize};

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody};
use crate::auth::{Human, valid_id};
use crate::db::{Db, now_ms};

/// Longest uploaded avatar, in bytes.
pub const MAX_AVATAR_BYTES: usize = 1024 * 1024;
/// Largest uploaded image side, in pixels.
pub const MAX_SOURCE_SIDE: u32 = 4096;
/// Largest stored avatar side, in pixels.
pub const AVATAR_SIDE: u32 = 256;
/// Longest bio, in characters.
pub const MAX_BIO_CHARS: usize = 280;
/// Most memory the decoder may allocate for one upload.
const DECODE_ALLOC: u64 = 96 * 1024 * 1024;
/// Avatar re-encodes at once across the server (so at most this many
/// times [`DECODE_ALLOC`] of decoder memory).
pub const MAX_AVATAR_ENCODES: usize = 2;

const NO_PROFILE: &str = "no such profile";
const NO_AVATAR: &str = "no avatar";

pub(crate) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS profiles (
             user_id        TEXT PRIMARY KEY NOT NULL,
             display_name   TEXT,
             default_name   TEXT,
             bio            TEXT,
             avatar_png     BLOB,
             avatar_version TEXT,
             updated_at     INTEGER NOT NULL
         );
         CREATE VIEW IF NOT EXISTS directory_cards AS
             SELECT p.user_id AS id,
                    coalesce(p.display_name, m.name, p.default_name, p.user_id) AS display_name,
                    p.avatar_version
             FROM profiles p LEFT JOIN managed_users m ON m.id = p.user_id;",
    )
}

/// A profile as clients see it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub display_name: String,
    pub bio: Option<String>,
    pub avatar_version: Option<String>,
}

// ---- Database ----

impl Db {
    /// The display name `id` uses now, recording `default_name` (what their
    /// authenticator calls them) when it changed. One read per request; a
    /// write only when the default changed.
    pub fn resolve_display_name(&self, id: &str, default_name: &str) -> anyhow::Result<String> {
        let conn = self.lock();
        let row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT display_name, default_name FROM profiles WHERE user_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (display, stored_default) = row.unwrap_or((None, None));
        if stored_default.as_deref() != Some(default_name) {
            conn.execute(
                "INSERT INTO profiles (user_id, default_name, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (user_id) DO UPDATE SET default_name = excluded.default_name",
                params![id, default_name, now_ms()],
            )?;
        }
        Ok(display.unwrap_or_else(|| default_name.to_owned()))
    }

    /// `id`'s profile as anyone may see it: their override, else a managed
    /// account's name, else the name they last signed in with, else the id.
    pub fn profile(&self, id: &str) -> anyhow::Result<Profile> {
        let conn = self.lock();
        profile_in(&conn, id)
    }

    /// `target`'s profile if `reader` may see it right now; the access
    /// check and the read share one lock and one read transaction, so a
    /// removal can't land between them.
    pub fn visible_profile(&self, reader: &str, target: &str) -> anyhow::Result<Option<Profile>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let bio = may_see_bio(&tx, reader, target)?;
        // Keep the legacy fallback card for a shared-Space member who hasn't
        // signed in yet. It doesn't make that member a directory entry.
        if !bio && !may_see_card(&tx, reader, target)? {
            return Ok(None);
        }
        let mut profile = profile_in(&tx, target)?;
        if !bio {
            profile.bio = None;
        }
        Ok(Some(profile))
    }

    /// `target`'s avatar if `reader` may see their profile right now:
    /// `None` when they may not, `Some(None)` when there is no avatar.
    /// Checked and read like [`Db::visible_profile`].
    pub fn visible_avatar(
        &self,
        reader: &str,
        target: &str,
    ) -> anyhow::Result<Option<Option<Vec<u8>>>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if !may_see_card(&tx, reader, target)? && !may_see_bio(&tx, reader, target)? {
            return Ok(None);
        }
        Ok(Some(avatar_in(&tx, target)?))
    }

    pub fn avatar(&self, id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let conn = self.lock();
        avatar_in(&conn, id)
    }

    /// Store a re-encoded avatar under a fresh version.
    pub fn set_avatar(&self, id: &str, png: &[u8]) -> anyhow::Result<Profile> {
        let version = random_hex(16)?;
        let conn = self.lock();
        conn.execute(
            "INSERT INTO profiles (user_id, avatar_png, avatar_version, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (user_id) DO UPDATE SET avatar_png = excluded.avatar_png,
                 avatar_version = excluded.avatar_version, updated_at = excluded.updated_at",
            params![id, png, version, now_ms()],
        )?;
        profile_in(&conn, id)
    }

    pub fn clear_avatar(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE profiles SET avatar_png = NULL, avatar_version = NULL, updated_at = ?2
             WHERE user_id = ?1",
            params![id, now_ms()],
        )?;
        Ok(())
    }
}

fn may_see_card(conn: &Connection, reader: &str, target: &str) -> rusqlite::Result<bool> {
    Ok(reader == target || crate::directory::listed(conn, target)?)
}

/// Bios are for the person themself, current Space peers and DM peers.
fn may_see_bio(conn: &Connection, reader: &str, target: &str) -> rusqlite::Result<bool> {
    if reader == target {
        return Ok(true);
    }
    if conn
        .query_row(
            "SELECT 1 FROM space_members a JOIN space_members b ON a.space_id = b.space_id
             WHERE a.user_id = ?1 AND b.user_id = ?2 LIMIT 1",
            params![reader, target],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Ok(true);
    }
    crate::dms::are_peers(conn, reader, target)
}

fn avatar_in(conn: &Connection, id: &str) -> anyhow::Result<Option<Vec<u8>>> {
    Ok(conn
        .query_row(
            "SELECT avatar_png FROM profiles WHERE user_id = ?1 AND avatar_png IS NOT NULL",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}

/// `display_name, default_name, bio, avatar_version` of a profile row.
type ProfileRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub(crate) fn profile_in(conn: &Connection, id: &str) -> anyhow::Result<Profile> {
    let row: Option<ProfileRow> = conn
        .query_row(
            "SELECT display_name, default_name, bio, avatar_version FROM profiles WHERE user_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (display, default, bio, avatar_version) = row.unwrap_or((None, None, None, None));
    let managed: Option<String> = conn
        .query_row("SELECT name FROM managed_users WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(Profile {
        id: id.to_owned(),
        display_name: display
            .or(managed)
            .or(default)
            .unwrap_or_else(|| id.to_owned()),
        bio,
        avatar_version,
    })
}

fn random_hex(n: usize) -> anyhow::Result<String> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no system randomness: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// What a PATCH changes.
pub enum Change<T> {
    Keep,
    Clear,
    Set(T),
}

/// Why a profile update was refused.
pub enum UpdateError {
    NameTaken,
    Other(anyhow::Error),
}

impl From<rusqlite::Error> for UpdateError {
    fn from(e: rusqlite::Error) -> Self {
        UpdateError::Other(e.into())
    }
}

impl From<anyhow::Error> for UpdateError {
    fn from(e: anyhow::Error) -> Self {
        UpdateError::Other(e)
    }
}

impl Db {
    /// Apply a PATCH in one transaction, checking the name under the lock.
    pub fn update_profile(
        &self,
        managed: &crate::admin_users::Managed,
        org_admins: &[String],
        id: &str,
        display_name: Change<String>,
        bio: Change<String>,
    ) -> Result<Profile, UpdateError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO profiles (user_id, updated_at) VALUES (?1, ?2)
             ON CONFLICT (user_id) DO NOTHING",
            params![id, now_ms()],
        )?;
        match display_name {
            Change::Keep => {}
            Change::Clear => {
                tx.execute(
                    "UPDATE profiles SET display_name = NULL WHERE user_id = ?1",
                    [id],
                )?;
            }
            Change::Set(name) => {
                if crate::admin_users::name_clash(&tx, managed, org_admins, &name, Some(id))? {
                    return Err(UpdateError::NameTaken);
                }
                tx.execute(
                    "UPDATE profiles SET display_name = ?2 WHERE user_id = ?1",
                    params![id, name],
                )?;
            }
        }
        match bio {
            Change::Keep => {}
            Change::Clear => {
                tx.execute("UPDATE profiles SET bio = NULL WHERE user_id = ?1", [id])?;
            }
            Change::Set(bio) => {
                tx.execute(
                    "UPDATE profiles SET bio = ?2 WHERE user_id = ?1",
                    params![id, bio],
                )?;
            }
        }
        tx.execute(
            "UPDATE profiles SET updated_at = ?2 WHERE user_id = ?1",
            params![id, now_ms()],
        )?;
        let profile = profile_in(&tx, id)?;
        tx.commit()?;
        Ok(profile)
    }
}

// ---- Validation ----

/// A bio: trimmed, at most [`MAX_BIO_CHARS`], line breaks allowed, no other
/// control or invisible direction characters. Empty clears it.
fn check_bio(bio: &str) -> ApiResult<Change<String>> {
    let bio = bio.replace("\r\n", "\n");
    let bio = bio.trim();
    if bio.is_empty() {
        return Ok(Change::Clear);
    }
    if bio.chars().count() > MAX_BIO_CHARS
        || bio
            .chars()
            .any(|c| (c.is_control() && c != '\n') || crate::admin_users::deceptive(c))
    {
        return Err(ApiError::bad_request(format!(
            "bio must be at most {MAX_BIO_CHARS} printable characters"
        )));
    }
    Ok(Change::Set(bio.to_owned()))
}

/// Re-encode an uploaded PNG or JPEG as a clean, bounded PNG.
pub fn reencode_avatar(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    use image::{ImageFormat, ImageReader, Limits};
    use std::io::Cursor;

    const BAD: &str = "the avatar must be a PNG or JPEG image";
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err("the avatar is too large");
    }
    let format = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageFormat::Png
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ImageFormat::Jpeg
    } else {
        return Err(BAD);
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(DECODE_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| match e {
        image::ImageError::Limits(_) => "the avatar's dimensions are too large",
        _ => BAD,
    })?;
    if image.width() == 0 || image.height() == 0 {
        return Err(BAD);
    }
    let image = if image.width() > AVATAR_SIDE || image.height() > AVATAR_SIDE {
        image.resize(
            AVATAR_SIDE,
            AVATAR_SIDE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };
    let rgba = image::DynamicImage::ImageRgba8(image.to_rgba8());
    let mut out = Cursor::new(Vec::new());
    rgba.write_to(&mut out, ImageFormat::Png).map_err(|_| BAD)?;
    Ok(out.into_inner())
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBody {
    #[serde(default, deserialize_with = "nullable")]
    display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    bio: Option<Option<String>>,
}

// ---- HTTP ----

fn private_json(status: StatusCode, profile: &Profile) -> Response {
    let mut response = (status, Json(profile)).into_response();
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn png_response(png: Vec<u8>) -> Response {
    let mut response = png.into_response();
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline; filename=\"avatar.png\""),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    response
}

pub async fn get_me(State(state): State<AppState>, who: Human) -> ApiResult<Response> {
    let id = who.id.clone();
    let profile = crate::api::db(&state, move |db| db.profile(&id)).await?;
    Ok(private_json(StatusCode::OK, &profile))
}

pub async fn patch_me(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<PatchBody>,
) -> ApiResult<Response> {
    let Json(body) = body?;
    let display_name = match body.display_name {
        None => Change::Keep,
        Some(None) => Change::Clear,
        Some(Some(name)) => Change::Set(crate::admin_users::check_name(&name)?),
    };
    let bio = match body.bio {
        None => Change::Keep,
        Some(None) => Change::Clear,
        Some(Some(bio)) => check_bio(&bio)?,
    };
    let (managed, admins, id) = (
        state.managed.clone(),
        state.config.org_admins.clone(),
        who.id.clone(),
    );
    let updated = state
        .db
        .call(move |db| Ok(db.update_profile(&managed, &admins, &id, display_name, bio)))
        .await
        .map_err(ApiError::internal)?;
    match updated {
        Ok(profile) => Ok(private_json(StatusCode::OK, &profile)),
        Err(UpdateError::NameTaken) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "that name is already used or reserved; choose another",
        )),
        Err(UpdateError::Other(e)) => Err(ApiError::internal(e)),
    }
}

pub async fn put_avatar(
    State(state): State<AppState>,
    who: Human,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let declared = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });
    // What the bytes are; anything else is left for the decoder to refuse.
    let sniffed = if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if body.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    };
    match (declared.as_deref(), sniffed) {
        (Some(d @ ("image/png" | "image/jpeg")), Some(s)) if d != s => {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "the avatar's bytes don't match its Content-Type",
            ));
        }
        (Some("image/png" | "image/jpeg"), _) => {}
        _ => {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "upload the avatar as image/png or image/jpeg",
            ));
        }
    }
    if body.len() > MAX_AVATAR_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the avatar is too large",
        ));
    }
    // Each decode may use up to DECODE_ALLOC, so only a few run at once;
    // more are refused at once instead of queueing. The permit moves into
    // the blocking task and is released only when the work there ends,
    // even if the client goes away and this handler is dropped.
    let Ok(permit) = state.avatar_encodes.clone().try_acquire_owned() else {
        let mut busy =
            ApiError::unavailable("avatar uploads are busy; try again shortly").into_response();
        busy.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
        return Ok(busy);
    };
    let png = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        reencode_avatar(&body)
    })
    .await
    .map_err(|e| ApiError::internal(e.into()))?
    .map_err(|reason| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, reason))?;
    let id = who.id.clone();
    let profile = crate::api::db(&state, move |db| db.set_avatar(&id, &png)).await?;
    Ok(private_json(StatusCode::OK, &profile))
}

pub async fn delete_avatar(State(state): State<AppState>, who: Human) -> ApiResult<Response> {
    let id = who.id.clone();
    crate::api::db(&state, move |db| db.clear_avatar(&id)).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub async fn get_my_avatar(State(state): State<AppState>, who: Human) -> ApiResult<Response> {
    let id = who.id.clone();
    match crate::api::db(&state, move |db| db.avatar(&id)).await? {
        Some(png) => Ok(png_response(png)),
        None => Err(ApiError::not_found(NO_AVATAR)),
    }
}

pub async fn get_profile(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    if !valid_id(&id) {
        return Err(ApiError::not_found(NO_PROFILE));
    }
    let reader = who.id.clone();
    match crate::api::db(&state, move |db| db.visible_profile(&reader, &id)).await? {
        Some(profile) => Ok(private_json(StatusCode::OK, &profile)),
        None => Err(ApiError::not_found(NO_PROFILE)),
    }
}

pub async fn get_avatar(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    if !valid_id(&id) {
        return Err(ApiError::not_found(NO_PROFILE));
    }
    let reader = who.id.clone();
    match crate::api::db(&state, move |db| db.visible_avatar(&reader, &id)).await? {
        Some(Some(png)) => Ok(png_response(png)),
        // Only someone who may see the profile learns there is no avatar.
        Some(None) => Err(ApiError::not_found(NO_AVATAR)),
        None => Err(ApiError::not_found(NO_PROFILE)),
    }
}

/// Profile routes, with full paths. The avatar upload sets its own limit.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/profile", get(get_me).patch(patch_me))
        .route(
            "/v1/me/profile/avatar",
            get(get_my_avatar)
                .put(put_avatar)
                .delete(delete_avatar)
                .layer(DefaultBodyLimit::max(MAX_AVATAR_BYTES)),
        )
        .route("/v1/profiles/{id}", get(get_profile))
        .route("/v1/profiles/{id}/avatar", get(get_avatar))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn avatars_are_reencoded_and_bounded() {
        let out = reencode_avatar(&png(600, 300)).unwrap();
        let img = image::load_from_memory_with_format(&out, image::ImageFormat::Png).unwrap();
        assert_eq!((img.width(), img.height()), (256, 128));
        // Small images keep their size; trailing bytes are gone.
        let mut small = png(16, 16);
        small.extend_from_slice(b"<script>alert(1)</script>");
        let out = reencode_avatar(&small).unwrap();
        assert!(!out.windows(8).any(|w| w == b"<script>"));
        assert!(reencode_avatar(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
        assert!(reencode_avatar(b"GIF89a....").is_err());
        assert!(reencode_avatar(b"\x89PNG\r\n\x1a\ngarbage").is_err());
        assert!(reencode_avatar(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]).is_err());
    }

    #[test]
    fn huge_dimensions_are_refused_before_decoding() {
        // A valid PNG header claiming 100000 x 100000 pixels.
        let header = png(1, 1);
        let mut bomb = header.clone();
        // IHDR width/height live at bytes 16..24; fix the CRC after.
        bomb[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        bomb[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        let crc = crc32(&bomb[12..29]);
        bomb[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(
            reencode_avatar(&bomb),
            Err("the avatar's dimensions are too large")
        );
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn bios_are_checked() {
        assert!(matches!(check_bio("  "), Ok(Change::Clear)));
        assert!(matches!(check_bio("a\r\nb"), Ok(Change::Set(b)) if b == "a\nb"));
        assert!(check_bio("a\u{0007}b").is_err());
        assert!(check_bio("a\u{202E}b").is_err());
        assert!(check_bio(&"x".repeat(MAX_BIO_CHARS + 1)).is_err());
        assert!(matches!(check_bio("<b>hi</b>"), Ok(Change::Set(b)) if b == "<b>hi</b>"));
    }
}
