use alloc::{format, string::String, vec::Vec};
use serde::Deserialize;

use crate::{AlbumRef, Artist, Playlist, Profile, Song};

pub const DEFAULT_API_BASE: &str = "http://127.0.0.1:3000";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiRequest {
    pub path: String,
    pub cookie: Option<String>,
}

impl ApiRequest {
    pub fn url(&self, base: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), self.path)
    }
}

pub fn qr_key() -> ApiRequest {
    request("/login/qr/key?timestamp=1")
}

pub fn qr_create(key: &str) -> ApiRequest {
    request(&format!("/login/qr/create?key={}&qrimg=false", encode(key)))
}

pub fn qr_check(key: &str, nonce: u64) -> ApiRequest {
    request(&format!(
        "/login/qr/check?key={}&timestamp={nonce}",
        encode(key)
    ))
}

pub fn login_status(cookie: &str, nonce: u64) -> ApiRequest {
    authenticated(format!("/login/status?timestamp={nonce}"), cookie)
}

pub fn user_playlists(uid: u64, offset: u32, cookie: &str) -> ApiRequest {
    authenticated(
        format!("/user/playlist?uid={uid}&limit=20&offset={offset}"),
        cookie,
    )
}

pub fn daily_playlists(cookie: &str, nonce: u64) -> ApiRequest {
    authenticated(format!("/recommend/resource?timestamp={nonce}"), cookie)
}

pub fn daily_songs(cookie: &str, nonce: u64) -> ApiRequest {
    authenticated(format!("/recommend/songs?timestamp={nonce}"), cookie)
}

pub fn playlist_detail(id: u64, cookie: &str, nonce: u64) -> ApiRequest {
    authenticated(
        format!("/playlist/detail?id={id}&s=0&timestamp={nonce}"),
        cookie,
    )
}

pub fn playlist_tracks(id: u64, offset: u32, cookie: &str) -> ApiRequest {
    authenticated(
        format!("/playlist/track/all?id={id}&limit=30&offset={offset}"),
        cookie,
    )
}

pub fn song_url(id: u64, cookie: &str, nonce: u64) -> ApiRequest {
    authenticated(
        format!("/song/url/v1?id={id}&level=standard&timestamp={nonce}"),
        cookie,
    )
}

pub fn lyric(id: u64, cookie: &str, nonce: u64) -> ApiRequest {
    authenticated(format!("/lyric/new?id={id}&timestamp={nonce}"), cookie)
}

pub fn search_songs(query: &str, offset: u32, cookie: Option<&str>) -> ApiRequest {
    with_cookie(
        format!(
            "/search?keywords={}&type=1&limit=20&offset={offset}",
            encode(query)
        ),
        cookie,
    )
}

pub fn search_artists(query: &str, offset: u32, cookie: Option<&str>) -> ApiRequest {
    with_cookie(
        format!(
            "/search?keywords={}&type=100&limit=10&offset={offset}",
            encode(query)
        ),
        cookie,
    )
}

pub fn artist_detail(id: u64, cookie: Option<&str>) -> ApiRequest {
    with_cookie(format!("/artist/detail?id={id}"), cookie)
}

pub fn artist_songs(id: u64, offset: u32, cookie: Option<&str>) -> ApiRequest {
    with_cookie(
        format!("/artist/songs?id={id}&order=hot&limit=30&offset={offset}"),
        cookie,
    )
}

pub fn artist_albums(id: u64, offset: u32, cookie: Option<&str>) -> ApiRequest {
    with_cookie(
        format!("/artist/album?id={id}&limit=20&offset={offset}"),
        cookie,
    )
}

fn request(path: &str) -> ApiRequest {
    ApiRequest {
        path: path.into(),
        cookie: None,
    }
}

fn authenticated(path: String, cookie: &str) -> ApiRequest {
    ApiRequest {
        path,
        cookie: Some(cookie.into()),
    }
}

fn with_cookie(path: String, cookie: Option<&str>) -> ApiRequest {
    ApiRequest {
        path,
        cookie: cookie.map(Into::into),
    }
}

fn encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApiError {
    Json,
    Server(i32),
    Missing,
}

#[derive(Deserialize)]
struct CodeOnly {
    code: i32,
}

fn ensure_ok(json: &str) -> Result<(), ApiError> {
    let code: CodeOnly = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if (200..300).contains(&code.code) || matches!(code.code, 800..=803) {
        Ok(())
    } else {
        Err(ApiError::Server(code.code))
    }
}

#[derive(Deserialize)]
struct QrKeyEnvelope {
    code: i32,
    data: QrKeyData,
}
#[derive(Deserialize)]
struct QrKeyData {
    unikey: String,
}

pub fn parse_qr_key(json: &str) -> Result<String, ApiError> {
    let value: QrKeyEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.data.unikey)
}

#[derive(Deserialize)]
struct QrCreateEnvelope {
    code: i32,
    data: QrCreateData,
}
#[derive(Deserialize)]
struct QrCreateData {
    qrurl: String,
}

pub fn parse_qr_url(json: &str) -> Result<String, ApiError> {
    let value: QrCreateEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.data.qrurl)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrCheck {
    pub code: i32,
    pub cookie: Option<String>,
}

#[derive(Deserialize)]
struct QrCheckEnvelope {
    code: i32,
    #[serde(default)]
    cookie: Option<String>,
}

pub fn parse_qr_check(json: &str) -> Result<QrCheck, ApiError> {
    let value: QrCheckEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    Ok(QrCheck {
        code: value.code,
        cookie: value.cookie,
    })
}

#[derive(Deserialize)]
struct LoginStatusEnvelope {
    data: LoginStatusData,
}
#[derive(Deserialize)]
struct LoginStatusData {
    code: i32,
    profile: Option<Profile>,
}

pub fn parse_profile(json: &str) -> Result<Profile, ApiError> {
    let value: LoginStatusEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.data.code != 200 {
        return Err(ApiError::Server(value.data.code));
    }
    value.data.profile.ok_or(ApiError::Missing)
}

#[derive(Deserialize)]
struct PlaylistsEnvelope {
    code: i32,
    #[serde(default, alias = "recommend")]
    playlist: Vec<Playlist>,
}

pub fn parse_playlists(json: &str) -> Result<Vec<Playlist>, ApiError> {
    let value: PlaylistsEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.playlist)
}

#[derive(Deserialize)]
struct PlaylistEnvelope {
    code: i32,
    playlist: Playlist,
}

pub fn parse_playlist(json: &str) -> Result<Playlist, ApiError> {
    let value: PlaylistEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.playlist)
}

#[derive(Deserialize)]
struct SongsEnvelope {
    code: i32,
    #[serde(default)]
    songs: Vec<Song>,
}

pub fn parse_songs(json: &str) -> Result<Vec<Song>, ApiError> {
    let value: SongsEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.songs)
}

#[derive(Deserialize)]
struct DailySongsEnvelope {
    code: i32,
    data: DailySongsData,
}
#[derive(Deserialize)]
struct DailySongsData {
    #[serde(default, alias = "dailySongs")]
    daily_songs: Vec<Song>,
}

pub fn parse_daily_songs(json: &str) -> Result<Vec<Song>, ApiError> {
    let value: DailySongsEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.data.daily_songs)
}

#[derive(Deserialize)]
struct SearchEnvelope {
    code: i32,
    result: SearchData,
}
#[derive(Default, Deserialize)]
struct SearchData {
    #[serde(default)]
    songs: Vec<Song>,
    #[serde(default)]
    artists: Vec<Artist>,
}

pub fn parse_search_songs(json: &str) -> Result<Vec<Song>, ApiError> {
    let value: SearchEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.result.songs)
}

pub fn parse_search_artists(json: &str) -> Result<Vec<Artist>, ApiError> {
    let value: SearchEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.result.artists)
}

pub fn parse_artist_songs(json: &str) -> Result<Vec<Song>, ApiError> {
    parse_songs(json)
}

#[derive(Deserialize)]
struct ArtistAlbumsEnvelope {
    code: i32,
    #[serde(default, alias = "hotAlbums")]
    hot_albums: Vec<AlbumRef>,
}

pub fn parse_artist_albums(json: &str) -> Result<Vec<AlbumRef>, ApiError> {
    let value: ArtistAlbumsEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.hot_albums)
}

#[derive(Deserialize)]
struct SongUrlEnvelope {
    code: i32,
    data: Vec<SongUrlData>,
}
#[derive(Deserialize)]
struct SongUrlData {
    url: Option<String>,
    #[serde(default, alias = "proxyUrl")]
    proxy_url: String,
}

pub fn parse_song_url(json: &str) -> Result<String, ApiError> {
    let value: SongUrlEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    let item = value.data.into_iter().next().ok_or(ApiError::Missing)?;
    if !item.proxy_url.is_empty() {
        Ok(item.proxy_url)
    } else {
        item.url.ok_or(ApiError::Missing)
    }
}

#[derive(Deserialize)]
struct ArtistDetailEnvelope {
    code: i32,
    data: ArtistDetailData,
}
#[derive(Deserialize)]
struct ArtistDetailData {
    artist: Artist,
}

pub fn parse_artist(json: &str) -> Result<Artist, ApiError> {
    let value: ArtistDetailEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    Ok(value.data.artist)
}

pub fn validate_response(json: &str) -> Result<(), ApiError> {
    ensure_ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_encode_queries_and_attach_cookie() {
        let request = search_songs("宇多田 ヒカル", 20, Some("MUSIC_U=abc"));
        assert!(request.path.contains("%E5%AE%87"));
        assert!(request.path.contains("offset=20"));
        assert_eq!(request.cookie.as_deref(), Some("MUSIC_U=abc"));
    }

    #[test]
    fn parses_login_and_song_url() {
        assert_eq!(
            parse_qr_key(r#"{"code":200,"data":{"unikey":"key-1"}}"#).unwrap(),
            "key-1"
        );
        assert_eq!(
            parse_song_url(r#"{"code":200,"data":[{"url":"https://a/song.mp3"}]}"#).unwrap(),
            "https://a/song.mp3"
        );
    }
}
