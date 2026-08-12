use alloc::{format, string::String, vec, vec::Vec};
use aes::Aes128;
use cipher::{BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use md5::{Digest, Md5};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AlbumRef, Artist, Playlist, Profile, Song};

pub const DEFAULT_API_BASE: &str = "https://interfacepc.music.163.com";
const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";
const EAPI_DELIMITER: &str = "-36cd479b6b5-";
const USER_AGENT: &str =
    "NeteaseMusic 9.0.90/5038 (iPhone; iOS 16.2; zh_CN)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiRequest {
    pub url: String,
    pub method: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

pub fn qr_key() -> ApiRequest {
    eapi("/api/login/qrcode/unikey", json!({ "type": 3 }), None, 1)
}

pub fn qr_url(key: &str) -> String {
    format!("https://music.163.com/login?codekey={}", encode(key))
}

pub fn qr_check(key: &str, nonce: u64) -> ApiRequest {
    eapi(
        "/api/login/qrcode/client/login",
        json!({ "key": key, "type": 3 }),
        None,
        nonce,
    )
}

pub fn login_status(cookie: &str, nonce: u64) -> ApiRequest {
    eapi("/api/w/nuser/account/get", json!({}), Some(cookie), nonce)
}

pub fn user_playlists(uid: u64, offset: u32, cookie: &str) -> ApiRequest {
    eapi(
        "/api/user/playlist",
        json!({ "uid": uid, "limit": 20, "offset": offset, "includeVideo": true }),
        Some(cookie),
        u64::from(offset) + 1,
    )
}

pub fn daily_playlists(cookie: &str, nonce: u64) -> ApiRequest {
    eapi(
        "/api/v1/discovery/recommend/resource",
        json!({}),
        Some(cookie),
        nonce,
    )
}

pub fn daily_songs(cookie: &str, nonce: u64) -> ApiRequest {
    eapi(
        "/api/v3/discovery/recommend/songs",
        json!({ "afresh": false }),
        Some(cookie),
        nonce,
    )
}

pub fn playlist_detail(id: u64, cookie: &str, nonce: u64) -> ApiRequest {
    eapi(
        "/api/v6/playlist/detail",
        json!({ "id": id, "n": 100000, "s": 0 }),
        Some(cookie),
        nonce,
    )
}

pub fn playlist_tracks(id: u64, offset: u32, cookie: &str) -> ApiRequest {
    eapi(
        "/api/v6/playlist/detail",
        json!({ "id": id, "n": 100000, "s": 0, "lyra_offset": offset }),
        Some(cookie),
        u64::from(offset) + 1,
    )
}

pub fn song_url(id: u64, cookie: &str, nonce: u64) -> ApiRequest {
    eapi(
        "/api/song/enhance/player/url",
        json!({ "ids": format!("[\"{id}\"]"), "br": 320000 }),
        Some(cookie),
        nonce,
    )
}

pub fn lyric(id: u64, cookie: &str, nonce: u64) -> ApiRequest {
    eapi(
        "/api/song/lyric/v1",
        json!({ "id": id, "cp": false, "tv": 0, "lv": 0, "rv": 0, "kv": 0, "yv": 0, "ytv": 0, "yrv": 0 }),
        Some(cookie),
        nonce,
    )
}

pub fn search_songs(query: &str, offset: u32, cookie: Option<&str>) -> ApiRequest {
    search(query, 1, 20, offset, cookie)
}

pub fn search_artists(query: &str, offset: u32, cookie: Option<&str>) -> ApiRequest {
    search(query, 100, 10, offset, cookie)
}

fn search(query: &str, kind: u32, limit: u32, offset: u32, cookie: Option<&str>) -> ApiRequest {
    eapi(
        "/api/search/get",
        json!({ "s": query, "type": kind, "limit": limit, "offset": offset }),
        cookie,
        u64::from(offset) + 1,
    )
}

pub fn artist_detail(id: u64, cookie: Option<&str>) -> ApiRequest {
    eapi("/api/artist/head/info/get", json!({ "id": id }), cookie, id)
}

pub fn artist_songs(id: u64, offset: u32, cookie: Option<&str>) -> ApiRequest {
    eapi(
        "/api/v1/artist/songs",
        json!({ "id": id, "private_cloud": "true", "work_type": 1, "order": "hot", "limit": 30, "offset": offset }),
        cookie,
        id.wrapping_add(u64::from(offset)),
    )
}

pub fn artist_albums(id: u64, offset: u32, cookie: Option<&str>) -> ApiRequest {
    eapi(
        &format!("/api/artist/albums/{id}"),
        json!({ "limit": 20, "offset": offset, "total": true }),
        cookie,
        id.wrapping_add(u64::from(offset)),
    )
}

fn eapi(path: &str, mut data: Value, cookie: Option<&str>, nonce: u64) -> ApiRequest {
    let header = json!({
        "osver": "16.2",
        "deviceId": "canopus-lyra-player",
        "os": "iPhone OS",
        "appver": "9.0.90",
        "versioncode": "5038",
        "buildver": "1700000000",
        "resolution": "336x480",
        "channel": "distribution",
        "requestId": format!("{nonce}_0000"),
    });
    if let Value::Object(fields) = &mut data {
        fields.insert("header".into(), header);
        fields.insert("e_r".into(), Value::Bool(false));
    }
    let text = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
    let digest = Md5::digest(
        format!("nobody{path}use{text}md5forencrypt").as_bytes(),
    );
    let signed = format!(
        "{path}{EAPI_DELIMITER}{text}{EAPI_DELIMITER}{digest:x}"
    );
    let mut bytes = signed.into_bytes();
    let message_len = bytes.len();
    bytes.resize(message_len + 16, 0);
    let encrypted = Aes128::new(EAPI_KEY.into())
        .encrypt_padded_mut::<Pkcs7>(&mut bytes, message_len)
        .unwrap_or(&[]);
    let mut params = String::with_capacity(encrypted.len() * 2);
    for byte in encrypted {
        params.push_str(&format!("{byte:02X}"));
    }
    let mut headers = vec![
        ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
        ("User-Agent".into(), USER_AGENT.into()),
    ];
    if let Some(cookie) = cookie.filter(|value| !value.is_empty()) {
        headers.push(("Cookie".into(), cookie.into()));
    }
    ApiRequest {
        url: format!("{DEFAULT_API_BASE}/eapi/{}", path.trim_start_matches("/api/")),
        method: "POST",
        headers,
        body: Some(format!("params={params}")),
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
    #[serde(default)]
    unikey: String,
    #[serde(default)]
    data: Option<QrKeyData>,
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
    if value.unikey.is_empty() {
        value.data.map(|data| data.unikey).ok_or(ApiError::Missing)
    } else {
        Ok(value.unikey)
    }
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
    #[serde(default)]
    playlist: Vec<Playlist>,
    #[serde(default)]
    recommend: Vec<Playlist>,
}

pub fn parse_playlists(json: &str) -> Result<Vec<Playlist>, ApiError> {
    let value: PlaylistsEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    if value.playlist.is_empty() {
        Ok(value.recommend)
    } else {
        Ok(value.playlist)
    }
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
    #[serde(default)]
    playlist: Option<PlaylistSongs>,
}

#[derive(Default, Deserialize)]
struct PlaylistSongs {
    #[serde(default)]
    tracks: Vec<Song>,
}

pub fn parse_songs(json: &str) -> Result<Vec<Song>, ApiError> {
    let value: SongsEnvelope = serde_json::from_str(json).map_err(|_| ApiError::Json)?;
    if value.code != 200 {
        return Err(ApiError::Server(value.code));
    }
    if value.songs.is_empty() {
        Ok(value.playlist.map(|playlist| playlist.tracks).unwrap_or_default())
    } else {
        Ok(value.songs)
    }
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
    fn endpoints_encrypt_posts_and_attach_cookie() {
        let request = search_songs("宇多田 ヒカル", 20, Some("MUSIC_U=abc"));
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "https://interfacepc.music.163.com/eapi/search/get");
        assert!(request.body.as_deref().unwrap_or("").starts_with("params="));
        assert!(request.headers.iter().any(|(key, value)| {
            key == "Cookie" && value == "MUSIC_U=abc"
        }));
        assert!(!request.body.as_deref().unwrap_or("").contains("宇多田"));
    }

    #[test]
    fn qr_url_is_local_and_encoded() {
        assert_eq!(
            qr_url("a+b"),
            "https://music.163.com/login?codekey=a%2Bb"
        );
    }

    #[test]
    fn direct_shapes_parse_without_node_envelopes() {
        let playlists = parse_playlists(
            r#"{"code":200,"recommend":[{"id":1,"name":"Daily"}]}"#,
        )
        .unwrap();
        assert_eq!(playlists[0].name, "Daily");
        let songs = parse_songs(
            r#"{"code":200,"playlist":{"tracks":[{"id":2,"name":"Track"}]}}"#,
        )
        .unwrap();
        assert_eq!(songs[0].name, "Track");
    }

    #[test]
    fn parses_login_and_song_url() {
        assert_eq!(
            parse_qr_key(r#"{"code":200,"data":{"unikey":"key-1"}}"#).unwrap(),
            "key-1"
        );
        assert_eq!(
            parse_qr_key(r#"{"code":200,"unikey":"key-2"}"#).unwrap(),
            "key-2"
        );
        assert_eq!(
            parse_song_url(r#"{"code":200,"data":[{"url":"https://a/song.mp3"}]}"#).unwrap(),
            "https://a/song.mp3"
        );
    }
}
