use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use aes::Aes128;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use cipher::{BlockEncrypt, BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use md5::{Digest, Md5};
use num_bigint::BigUint;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use waki::{Client, Method};

use crate::{
    artwork,
    import::ImportAsset,
    sniff,
    state::{CloudPlaylist, CloudSong},
};

const API_BASE: &str = "https://interfacepc.music.163.com";
const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";
const EAPI_DELIMITER: &str = "-36cd479b6b5-";
const USER_AGENT: &str = "NeteaseMusic 9.0.90/5038 (iPhone; iOS 16.2; zh_CN)";
const WEB_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0";
const WEAPI_NONCE: &[u8; 16] = b"0CoJUm6Qyw8W8jud";
const WEAPI_IV: &[u8; 16] = b"0102030405060708";
const WEAPI_MODULUS: &str = "e0b509f6259df8642dbc35662901477df22677ec152b5ff68ace615bb7b725152b3ab17a876aea8a5aa76d2e417629ec4ee341f56135fccf695280104e0312ecbda92557c93870114af6c9d05c4f7f0c3685b7a46bee255932575cce10b424d813cfe4875d3e82047b97ddef52741d546b8e289dc6935b3ece0462db0a22b8e7";
const MAX_AUDIO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COVER_BYTES: u64 = 4 * 1024 * 1024;

pub const AUDIO_BITRATE_LOW: u32 = 128_000;
pub const AUDIO_BITRATE_MEDIUM: u32 = 192_000;
pub const AUDIO_BITRATE_HIGH: u32 = 320_000;

pub fn normalized_audio_bitrate(value: u32) -> u32 {
    match value {
        AUDIO_BITRATE_LOW | AUDIO_BITRATE_MEDIUM | AUDIO_BITRATE_HIGH => value,
        _ => AUDIO_BITRATE_LOW,
    }
}

static DEVICE_ID: OnceLock<String> = OnceLock::new();
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);
static QR_SESSION: OnceLock<Mutex<QrSession>> = OnceLock::new();

#[derive(Clone, Debug)]
struct ApiRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct ApiResponse {
    body: Vec<u8>,
    cookies: Vec<String>,
}

#[derive(Default)]
struct QrSession {
    key: String,
    cookies: Vec<String>,
}

fn qr_session() -> &'static Mutex<QrSession> {
    QR_SESSION.get_or_init(|| Mutex::new(QrSession::default()))
}

#[derive(Deserialize)]
struct SearchEnvelope {
    code: i32,
    #[serde(default)]
    result: SearchResult,
}

#[derive(Default, Deserialize)]
struct SearchResult {
    #[serde(default)]
    songs: Vec<SongWire>,
}

#[derive(Deserialize)]
struct SongDetailEnvelope {
    code: i32,
    #[serde(default)]
    songs: Vec<SongWire>,
}

#[derive(Deserialize)]
struct AccountEnvelope {
    code: i32,
    #[serde(default)]
    profile: Option<AccountProfile>,
}

#[derive(Deserialize)]
struct AccountProfile {
    #[serde(alias = "userId")]
    user_id: u64,
}

#[derive(Deserialize)]
struct PlaylistEnvelope {
    code: i32,
    #[serde(default)]
    playlist: Vec<PlaylistWire>,
}

#[derive(Deserialize)]
struct PlaylistWire {
    id: u64,
    #[serde(deserialize_with = "string_or_empty")]
    name: String,
    #[serde(default, alias = "trackCount")]
    track_count: u32,
    #[serde(default)]
    creator: PlaylistCreatorWire,
}

#[derive(Default, Deserialize)]
struct PlaylistCreatorWire {
    #[serde(default, deserialize_with = "string_or_empty")]
    nickname: String,
}

#[derive(Deserialize)]
struct PlaylistDetailEnvelope {
    code: i32,
    #[serde(default)]
    playlist: PlaylistDetailWire,
}

#[derive(Default, Deserialize)]
struct PlaylistDetailWire {
    #[serde(default, deserialize_with = "string_or_empty")]
    name: String,
    #[serde(default)]
    tracks: Vec<SongWire>,
}

#[derive(Deserialize)]
struct SongWire {
    id: u64,
    #[serde(deserialize_with = "string_or_empty")]
    name: String,
    #[serde(default, alias = "artists")]
    ar: Vec<ArtistWire>,
    #[serde(default, alias = "album")]
    al: AlbumWire,
    #[serde(default, alias = "duration")]
    dt: u32,
}

#[derive(Deserialize)]
struct ArtistWire {
    #[serde(deserialize_with = "string_or_empty")]
    name: String,
}

#[derive(Default, Deserialize)]
struct AlbumWire {
    #[serde(default, deserialize_with = "string_or_empty")]
    name: String,
    #[serde(default, alias = "picUrl", deserialize_with = "string_or_empty")]
    pic_url: String,
}

fn string_or_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
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

#[derive(Deserialize)]
struct QrCheckEnvelope {
    code: i32,
    #[serde(default)]
    cookie: Option<String>,
}

pub struct PreparedCloud {
    pub song: CloudSong,
    pub palette: artwork::Palette,
    pub assets: Vec<ImportAsset>,
    pub warnings: Vec<String>,
    pub temp_directory: PathBuf,
}

pub fn search(query: &str, cookie: &str) -> Result<Vec<CloudSong>, String> {
    search_with(query, cookie, perform)
}

fn search_with(
    query: &str,
    cookie: &str,
    mut request: impl FnMut(ApiRequest) -> Result<Vec<u8>, String>,
) -> Result<Vec<CloudSong>, String> {
    let bytes = request(eapi(
        "/api/search/get",
        json!({ "s": query, "type": 1, "limit": 20, "offset": 0 }),
        cookie,
    ))?;
    let value: SearchEnvelope =
        serde_json::from_slice(&bytes).map_err(|error| format!("无法解析搜索结果：{error}"))?;
    if value.code != 200 {
        return Err(format!("网易云搜索失败：{}", value.code));
    }
    let mut songs: Vec<CloudSong> = value.result.songs.into_iter().map(cloud_song).collect();
    let ids: Vec<u64> = songs
        .iter()
        .filter(|song| song.cover_url.trim().is_empty())
        .map(|song| song.id)
        .collect();
    if !ids.is_empty() {
        let detail = song_details(&ids, cookie, &mut request);
        match detail {
            Ok(details) => {
                for detail in details {
                    if !detail.al.pic_url.trim().is_empty() {
                        for song in songs
                            .iter_mut()
                            .filter(|song| song.id == detail.id && song.cover_url.trim().is_empty())
                        {
                            song.cover_url = detail.al.pic_url.clone();
                        }
                    }
                }
            }
            Err(error) => tracing::warn!(error = %error, "NetEase search cover lookup failed"),
        }
    }
    Ok(songs)
}

fn song_details(
    ids: &[u64],
    cookie: &str,
    request: &mut impl FnMut(ApiRequest) -> Result<Vec<u8>, String>,
) -> Result<Vec<SongWire>, String> {
    let data = json!({ "c": json!(ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>()).to_string() });
    let bytes = request(eapi("/api/v3/song/detail", data, cookie))?;
    let value: SongDetailEnvelope =
        serde_json::from_slice(&bytes).map_err(|error| format!("无法解析歌曲详情：{error}"))?;
    if value.code != 200 {
        return Err(format!("歌曲详情请求失败：{}", value.code));
    }
    Ok(value.songs)
}

pub fn user_playlists(cookie: &str) -> Result<Vec<CloudPlaylist>, String> {
    if cookie.trim().is_empty() {
        return Err("请先扫码登录或填写网易云 Cookie。".to_string());
    }
    let account_bytes = perform(eapi("/api/nuser/account/get", json!({}), cookie))?;
    let account: AccountEnvelope = serde_json::from_slice(&account_bytes)
        .map_err(|error| format!("无法解析网易云账号：{error}"))?;
    if account.code != 200 {
        return Err(format!("网易云账号请求失败：{}", account.code));
    }
    let uid = account
        .profile
        .map(|profile| profile.user_id)
        .ok_or_else(|| "Cookie 已失效或未登录网易云账号。".to_string())?;
    let bytes = perform(eapi(
        "/api/user/playlist",
        json!({ "uid": uid, "limit": 100, "offset": 0, "includeVideo": true }),
        cookie,
    ))?;
    let value: PlaylistEnvelope =
        serde_json::from_slice(&bytes).map_err(|error| format!("无法解析个人歌单：{error}"))?;
    if value.code != 200 {
        return Err(format!("个人歌单请求失败：{}", value.code));
    }
    Ok(value
        .playlist
        .into_iter()
        .map(|playlist| CloudPlaylist {
            id: playlist.id,
            name: playlist.name,
            creator: playlist.creator.nickname,
            track_count: playlist.track_count,
        })
        .collect())
}

pub fn playlist_tracks(id: u64, cookie: &str) -> Result<(String, Vec<CloudSong>), String> {
    let bytes = perform(eapi(
        "/api/v6/playlist/detail",
        json!({ "id": id, "n": 1000, "s": 8 }),
        cookie,
    ))?;
    let value: PlaylistDetailEnvelope =
        serde_json::from_slice(&bytes).map_err(|error| format!("无法解析歌单歌曲：{error}"))?;
    if value.code != 200 {
        return Err(format!("歌单详情请求失败：{}", value.code));
    }
    Ok((
        value.playlist.name,
        value.playlist.tracks.into_iter().map(cloud_song).collect(),
    ))
}

fn cloud_song(song: SongWire) -> CloudSong {
    CloudSong {
        id: song.id,
        name: song.name,
        artists: song.ar.into_iter().map(|artist| artist.name).collect(),
        album: song.al.name,
        duration_ms: song.dt,
        cover_url: song.al.pic_url,
    }
}

pub fn begin_qr_login() -> Result<(String, String), String> {
    let s_device_id = device_id().to_string();
    let initial_cookie = format!("sDeviceId={s_device_id}");
    let response = perform_response(weapi(
        "/api/login/qrcode/unikey",
        json!({ "type": 1, "noCheckToken": true }),
        &initial_cookie,
    )?)?;
    let value: QrKeyEnvelope = serde_json::from_slice(&response.body)
        .map_err(|error| format!("无法解析二维码登录响应：{error}"))?;
    if value.code != 200 {
        return Err(format!("二维码登录初始化失败：{}", value.code));
    }
    let key = if value.unikey.is_empty() {
        value.data.map(|data| data.unikey).unwrap_or_default()
    } else {
        value.unikey
    };
    if key.is_empty() {
        return Err("网易云未返回二维码 key".to_string());
    }
    let mut cookies = vec![initial_cookie];
    merge_cookies(&mut cookies, response.cookies);
    let timestamp = unix_millis();
    let chain_id = format!("v1_{s_device_id}_web_login_{timestamp}");
    let url = format!(
        "http://music.163.com/login?codekey={}&chainId={}",
        url_encode(&key),
        url_encode(&chain_id),
    );
    let mut session = qr_session().lock().unwrap_or_else(|item| item.into_inner());
    session.key = key.clone();
    session.cookies = cookies;
    Ok((key, url))
}

pub fn poll_qr_login(key: &str) -> Result<Option<String>, String> {
    let cookie = {
        let session = qr_session().lock().unwrap_or_else(|item| item.into_inner());
        if session.key != key {
            return Err("二维码登录会话已失效，请重新生成".to_string());
        }
        session.cookies.join("; ")
    };
    let response = perform_response(weapi(
        "/api/login/qrcode/client/login",
        json!({ "key": key, "type": 1, "noCheckToken": true }),
        &cookie,
    )?)?;
    let value: QrCheckEnvelope = serde_json::from_slice(&response.body)
        .map_err(|error| format!("无法解析扫码状态：{error}"))?;
    let mut session = qr_session().lock().unwrap_or_else(|item| item.into_inner());
    merge_cookies(&mut session.cookies, response.cookies);
    match value.code {
        800 => Err("二维码已过期，请重新生成".to_string()),
        801 | 802 => Ok(None),
        803 => {
            if let Some(cookie) = value.cookie.filter(|cookie| !cookie.is_empty()) {
                merge_cookies(&mut session.cookies, split_cookie_header(&cookie));
            }
            let cookie = session.cookies.join("; ");
            session.key.clear();
            if cookie.is_empty() {
                Err("登录成功但未返回 Cookie".to_string())
            } else {
                Ok(Some(cookie))
            }
        }
        8821 => Err("网易云触发了环境风险验证，请稍后重试或使用官方网页登录 Cookie".to_string()),
        code => Err(format!("扫码登录失败：{code}")),
    }
}

pub fn prepare(
    song: &CloudSong,
    cookie: &str,
    audio_bitrate: u32,
) -> Result<PreparedCloud, String> {
    let audio_bitrate = normalized_audio_bitrate(audio_bitrate);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = PathBuf::from("media").join(format!("netease-{}-{nonce}", song.id));
    prepare_with(song, cookie, audio_bitrate, &directory, perform, download)
}

fn prepare_with(
    song: &CloudSong,
    cookie: &str,
    audio_bitrate: u32,
    directory: &PathBuf,
    mut request: impl FnMut(ApiRequest) -> Result<Vec<u8>, String>,
    mut download_file: impl FnMut(&str, &PathBuf, u64) -> Result<u64, String>,
) -> Result<PreparedCloud, String> {
    if let Some(parent) = directory.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建下载目录：{error}"))?;
    }
    fs::create_dir(directory).map_err(|error| format!("无法创建下载目录：{error}"))?;
    let result = prepare_in_directory(
        song,
        cookie,
        audio_bitrate,
        directory,
        &mut request,
        &mut download_file,
    );
    if result.is_err() {
        if let Err(error) = fs::remove_dir_all(directory) {
            tracing::warn!(path = %directory.display(), error = %error, "Unable to remove failed NetEase download directory");
        }
    }
    result
}

fn prepare_in_directory(
    song: &CloudSong,
    cookie: &str,
    audio_bitrate: u32,
    directory: &PathBuf,
    request: &mut impl FnMut(ApiRequest) -> Result<Vec<u8>, String>,
    download_file: &mut impl FnMut(&str, &PathBuf, u64) -> Result<u64, String>,
) -> Result<PreparedCloud, String> {
    let url = retry("歌曲地址", || {
        let song_url_bytes = request(eapi(
            "/api/song/enhance/player/url",
            json!({ "ids": format!("[\"{}\"]", song.id), "br": audio_bitrate }),
            cookie,
        ))?;
        let song_url: SongUrlEnvelope = serde_json::from_slice(&song_url_bytes)
            .map_err(|error| format!("无法解析歌曲地址：{error}"))?;
        if song_url.code != 200 {
            return Err(format!("歌曲地址请求失败：{}", song_url.code));
        }
        let item = song_url
            .data
            .into_iter()
            .next()
            .ok_or_else(|| "网易云未返回歌曲地址".to_string())?;
        let url = if item.proxy_url.is_empty() {
            item.url.unwrap_or_default()
        } else {
            item.proxy_url
        };
        if url.trim().is_empty() {
            return Err("歌曲因版权或区域限制无法下载".to_string());
        }
        Ok(url)
    })?;
    let mut song = song.clone();
    let mut warnings = Vec::new();
    if song.cover_url.trim().is_empty() {
        match retry("歌曲封面详情", || {
            song_details(&[song.id], cookie, request)
        }) {
            Ok(details) => {
                if let Some(detail) = details.into_iter().find(|detail| detail.id == song.id) {
                    song.cover_url = detail.al.pic_url;
                }
                if song.cover_url.trim().is_empty() {
                    warnings.push("无法取得封面：歌曲详情未返回封面地址".to_string());
                }
            }
            Err(error) => warnings.push(error),
        }
    }

    let audio_path = directory.join("audio.mp3");
    retry("歌曲音频", || {
        let size = download_file(&url, &audio_path, MAX_AUDIO_BYTES)?;
        validate_audio(&audio_path)?;
        Ok(size)
    })?;
    let mut assets = vec![ImportAsset::audio(path_text(&audio_path)?)];

    let mut palette = artwork::Palette::default();
    if song.cover_url.trim().is_empty() {
        tracing::warn!(song_id = song.id, "NetEase song has no album cover URL");
    } else {
        let source_path = directory.join("cover-source.jpg");
        let cover_url = sized_cover_url(&song.cover_url);
        match retry("歌曲封面", || {
            let size = download_file(&cover_url, &source_path, MAX_COVER_BYTES)?;
            let prepared = artwork::prepare(&source_path, &directory)?;
            Ok((size, prepared))
        }) {
            Ok((size, prepared)) => {
                tracing::info!(
                    song_id = song.id,
                    source_bytes = size,
                    cover_bytes = prepared.cover_size,
                    "NetEase artwork prepared"
                );
                palette = prepared.palette;
                assets.push(ImportAsset::cover(prepared.cover_path));
            }
            Err(error) => {
                tracing::warn!(song_id = song.id, error = %error, "NetEase artwork download skipped");
                warnings.push(error);
            }
        }
    }

    let lyrics = retry("歌曲歌词", || {
        let bytes = request(eapi(
            "/api/song/lyric/v1",
            json!({ "id": song.id, "cp": false, "tv": 0, "lv": 0, "rv": 0, "kv": 0, "yv": 0, "ytv": 0, "yrv": 0 }),
            cookie,
        ))?;
        validate_lyrics(&bytes)?;
        Ok(bytes)
    });
    match lyrics {
        Ok(bytes) if validate_lyrics(&bytes)?.is_some() => {
            let lyrics_path = directory.join("lyrics.json");
            fs::write(&lyrics_path, &bytes).map_err(|error| format!("无法保存歌词：{error}"))?;
            assets.push(ImportAsset::lyrics(path_text(&lyrics_path)?, "json"));
        }
        Ok(_) => tracing::info!(song_id = song.id, "NetEase song has no lyrics"),
        Err(error) => {
            tracing::warn!(song_id = song.id, error = %error, "NetEase lyrics download skipped");
            warnings.push(error);
        }
    }

    Ok(PreparedCloud {
        song,
        palette,
        assets,
        warnings,
        temp_directory: directory.clone(),
    })
}

fn sized_cover_url(source: &str) -> String {
    let Ok(mut url) = url::Url::parse(source) else {
        return source.to_string();
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.host_str().is_some_and(|host| host == "music.126.net" || host.ends_with(".music.126.net"))
    {
        return source.to_string();
    }
    let pairs: Vec<(String, String)> = url.query_pairs()
        .filter(|(key, _)| key != "param")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(pairs).append_pair("param", "512y512");
    url.into()
}

fn validate_audio(path: &Path) -> Result<(), String> {
    if sniff::is_mp3(path).map_err(|error| format!("无法检查音频：{error}"))? {
        Ok(())
    } else {
        Err("下载内容不是有效的 MP3".to_string())
    }
}

fn retry<T>(label: &str, mut operation: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    const ATTEMPTS: usize = 3;
    for attempt in 1..=ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if attempt == ATTEMPTS => {
                return Err(format!("{label}失败（已尝试 {ATTEMPTS} 次）：{error}"));
            }
            Err(error) => {
                tracing::warn!(attempt, error = %error, resource = label, "NetEase request retry")
            }
        }
    }
    unreachable!()
}

fn validate_lyrics(bytes: &[u8]) -> Result<Option<()>, String> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("歌词超过大小限制".into());
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("歌词 JSON 无效：{error}"))?;
    if value.get("code").and_then(Value::as_i64) != Some(200) {
        return Err("歌词接口返回错误状态".into());
    }
    if value.get("nolyric").and_then(Value::as_bool) == Some(true)
        || value.get("uncollected").and_then(Value::as_bool) == Some(true)
    {
        return Ok(None);
    }
    let Some(lrc) = value.get("lrc") else {
        return Err("歌词响应缺少 lrc".into());
    };
    let lyric = lrc
        .get("lyric")
        .and_then(Value::as_str)
        .ok_or_else(|| "歌词内容无效".to_string())?;
    if lyric.trim().is_empty() {
        return Ok(None);
    }
    crate::lyrics::to_lrc(bytes, "json")?;
    Ok(Some(()))
}

fn perform(request: ApiRequest) -> Result<Vec<u8>, String> {
    perform_response(request).map(|response| response.body)
}

fn perform_response(request: ApiRequest) -> Result<ApiResponse, String> {
    let client = Client::new();
    let mut builder = client.request(Method::Post, &request.url);
    for (name, value) in request.headers {
        let name = waki::header::HeaderName::try_from(name.as_str())
            .map_err(|error| format!("无效 HTTP header：{error}"))?;
        builder = builder.header(name, value);
    }
    let response = builder
        .body(request.body)
        .send()
        .map_err(|error| format!("网易云请求失败：{error}"))?;
    let status = response.status_code();
    let cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(cookie_pair)
        .filter(|value| !value.is_empty())
        .collect();
    let body = response
        .body()
        .map_err(|error| format!("读取网易云响应失败：{error}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("网易云 HTTP {status}"));
    }
    Ok(ApiResponse { body, cookies })
}

fn download(url: &str, path: &PathBuf, limit: u64) -> Result<u64, String> {
    let client = Client::new();
    let response = client
        .request(Method::Get, url)
        .header(waki::header::USER_AGENT, USER_AGENT)
        .header(waki::header::REFERER, "https://music.163.com/")
        .header(waki::header::ACCEPT, "image/jpeg,image/png")
        .send()
        .map_err(|error| format!("下载失败：{error}"))?;
    if !(200..300).contains(&response.status_code()) {
        return Err(format!("下载 HTTP {}", response.status_code()));
    }
    let mut file = File::create(path).map_err(|error| format!("无法创建下载文件：{error}"))?;
    let mut total = 0u64;
    while let Some(bytes) = response
        .chunk(64 * 1024)
        .map_err(|error| format!("读取下载流失败：{error}"))?
    {
        total = total.saturating_add(bytes.len() as u64);
        if total > limit {
            let _ = fs::remove_file(path);
            return Err("下载内容超过导入上限".to_string());
        }
        file.write_all(&bytes)
            .map_err(|error| format!("写入下载文件失败：{error}"))?;
    }
    if total == 0 {
        let _ = fs::remove_file(path);
        return Err("下载内容为空".to_string());
    }
    Ok(total)
}

fn weapi(path: &str, mut data: Value, cookie: &str) -> Result<ApiRequest, String> {
    if let Value::Object(fields) = &mut data {
        fields.insert(
            "csrf_token".into(),
            Value::String(cookie_value(cookie, "__csrf")),
        );
    }
    let text =
        serde_json::to_vec(&data).map_err(|error| format!("无法编码 WEAPI 请求：{error}"))?;
    let secret = random_secret();
    let first = aes_cbc_base64(&text, WEAPI_NONCE)?;
    let params = aes_cbc_base64(first.as_bytes(), &secret)?;
    let modulus = BigUint::parse_bytes(WEAPI_MODULUS.as_bytes(), 16)
        .ok_or_else(|| "无效 WEAPI RSA modulus".to_string())?;
    let exponent = BigUint::from(65_537u32);
    let mut reversed = secret;
    reversed.reverse();
    let encrypted = BigUint::from_bytes_be(&reversed).modpow(&exponent, &modulus);
    let enc_sec_key = format!("{encrypted:0256x}");
    Ok(ApiRequest {
        url: format!(
            "https://music.163.com/weapi/{}",
            path.trim_start_matches("/api/")
        ),
        headers: vec![
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            ("Referer".into(), "https://music.163.com".into()),
            ("User-Agent".into(), WEB_USER_AGENT.into()),
            ("Cookie".into(), cookie.to_string()),
        ],
        body: format!("params={}&encSecKey={enc_sec_key}", url_encode(&params)).into_bytes(),
    })
}

fn aes_cbc_base64(input: &[u8], key: &[u8; 16]) -> Result<String, String> {
    let padding = 16 - input.len() % 16;
    let mut bytes = Vec::with_capacity(input.len() + padding);
    bytes.extend_from_slice(input);
    bytes.resize(input.len() + padding, padding as u8);
    let cipher = Aes128::new_from_slice(key).map_err(|_| "无效 WEAPI AES key".to_string())?;
    let mut previous = *WEAPI_IV;
    for block in bytes.chunks_exact_mut(16) {
        for (byte, prior) in block.iter_mut().zip(previous) {
            *byte ^= prior;
        }
        cipher.encrypt_block(block.into());
        previous.copy_from_slice(block);
    }
    Ok(BASE64.encode(bytes))
}

fn random_secret() -> [u8; 16] {
    const ALPHANUMERIC: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let nonce = unix_millis();
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let digest = Md5::digest(format!("{nonce}:{counter}").as_bytes());
    let mut secret = [0u8; 16];
    for (index, byte) in secret.iter_mut().enumerate() {
        *byte = ALPHANUMERIC[digest[index] as usize % ALPHANUMERIC.len()];
    }
    secret
}

fn eapi(path: &str, mut data: Value, cookie: &str) -> ApiRequest {
    let millis = unix_millis();
    let buildver = millis / 1_000;
    let request_id = format!(
        "{millis}_{:04}",
        ID_COUNTER.fetch_add(1, Ordering::Relaxed) % 1_000
    );
    let device_id = device_id();
    let header = json!({
        "osver": "Microsoft-Windows-10-Professional-build-19045-64bit",
        "deviceId": device_id,
        "os": "pc",
        "appver": "3.1.17.204416",
        "versioncode": "140",
        "mobilename": "",
        "buildver": buildver.to_string(),
        "resolution": "1920x1080",
        "__csrf": cookie_value(cookie, "__csrf"),
        "channel": "netease",
        "requestId": request_id.clone(),
    });
    if let Value::Object(fields) = &mut data {
        fields.insert("header".into(), header);
        fields.insert("e_r".into(), Value::Bool(false));
    }
    let text = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
    let digest = Md5::digest(format!("nobody{path}use{text}md5forencrypt").as_bytes());
    let signed = format!("{path}{EAPI_DELIMITER}{text}{EAPI_DELIMITER}{digest:x}");
    let mut bytes = signed.into_bytes();
    let message_len = bytes.len();
    bytes.resize(message_len + 16, 0);
    let encrypted = Aes128::new(EAPI_KEY.into())
        .encrypt_padded_mut::<Pkcs7>(&mut bytes, message_len)
        .unwrap_or(&[]);
    let params = encrypted
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    let identity_cookie = format!(
        "osver=Microsoft-Windows-10-Professional-build-19045-64bit; deviceId={device_id}; os=pc; appver=3.1.17.204416; versioncode=140; buildver={buildver}; resolution=1920x1080; channel=netease; requestId={request_id}"
    );
    let cookie_header = if cookie.trim().is_empty() {
        identity_cookie
    } else {
        format!("{}; {identity_cookie}", cookie.trim())
    };
    let headers = vec![
        (
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ),
        ("User-Agent".into(), USER_AGENT.into()),
        ("Cookie".into(), cookie_header),
    ];
    ApiRequest {
        url: format!("{API_BASE}/eapi/{}", path.trim_start_matches("/api/")),
        headers,
        body: format!("params={params}").into_bytes(),
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn device_id() -> &'static str {
    DEVICE_ID.get_or_init(|| {
        let nonce = unix_millis();
        let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let first = Md5::digest(format!("{nonce}:{counter}:0").as_bytes());
        let second = Md5::digest(format!("{counter}:{nonce}:1").as_bytes());
        let mut value = format!("{first:X}{second:X}");
        value.truncate(52);
        value
    })
}

fn cookie_pair(value: &str) -> String {
    value.split(';').next().unwrap_or("").trim().to_string()
}

fn split_cookie_header(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|item| item.contains('='))
        .filter(|item| {
            let name = item.split_once('=').map(|(name, _)| name).unwrap_or("");
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "domain" | "path" | "expires" | "max-age" | "samesite"
            )
        })
        .map(str::to_string)
        .collect()
}

fn merge_cookies(current: &mut Vec<String>, incoming: Vec<String>) {
    for cookie in incoming {
        let cookie = cookie_pair(&cookie);
        let Some((name, _)) = cookie.split_once('=') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        current.retain(|item| item.split_once('=').map(|(key, _)| key) != Some(name));
        current.push(cookie);
    }
}

fn cookie_value(cookie: &str, name: &str) -> String {
    cookie
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_string()))
        .unwrap_or_default()
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| "下载路径不是 UTF-8".to_string())
}

fn url_encode(input: &str) -> String {
    let mut output = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sized_cover_url_preserves_other_parameters_and_restricts_host() {
        assert_eq!(sized_cover_url("https://p1.music.126.net/a?x=1&param=96y96#part"),
                   "https://p1.music.126.net/a?x=1&param=512y512#part");
        for source in ["https://example.com/a", "https://music.126.net.example.com/a", "file:///a", "invalid"] {
            assert_eq!(sized_cover_url(source), source);
        }
    }

    #[test]
    fn prepare_requests_a_bounded_netease_cover() {
        let directory = artwork::unique_output_directory("netease-cover-size-test");
        let song = CloudSong {
            id: 7,
            name: "Track".into(),
            artists: vec![],
            album: String::new(),
            duration_ms: 0,
            cover_url: "https://p1.music.126.net/cover.jpg".into(),
        };
        let prepared = prepare_with(&song, "", AUDIO_BITRATE_LOW, &directory, |request| {
            if request.url.ends_with("/song/enhance/player/url") {
                Ok(br#"{"code":200,"data":[{"url":"https://audio"}]}"#.to_vec())
            } else {
                Ok(br#"{"code":200,"nolyric":true}"#.to_vec())
            }
        }, |url, path, _| {
            if url == "https://audio" {
                let mut frame = vec![0; 417];
                frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
                fs::write(path, frame.repeat(2)).unwrap();
                Ok(834)
            } else if url == "https://p1.music.126.net/cover.jpg?param=512y512" {
                image::RgbaImage::from_pixel(512, 512, image::Rgba([30, 120, 200, 255]))
                    .save_with_format(path, image::ImageFormat::Png).unwrap();
                Ok(fs::metadata(path).unwrap().len())
            } else {
                Err("original cover exceeds decode limit".into())
            }
        }).unwrap();
        assert!(prepared.warnings.is_empty(), "{:?}", prepared.warnings);
        assert!(directory.join("cover.bin").exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn search_fills_missing_covers_by_id_without_overwriting_existing_covers() {
        let mut calls = 0;
        let songs = search_with("test", "", |request| {
            calls += 1;
            if calls == 1 {
                assert_eq!(request.url, format!("{API_BASE}/eapi/search/get"));
                Ok(br#"{"code":200,"result":{"songs":[{"id":1,"name":"One","al":{"picUrl":"https://existing"}},{"id":2,"name":"Two"},{"id":3,"name":"Three"}]}}"#.to_vec())
            } else {
                assert_eq!(request.url, format!("{API_BASE}/eapi/v3/song/detail"));
                Ok(br#"{"code":200,"songs":[{"id":3,"name":"Other","al":{"picUrl":"https://three"}},{"id":1,"name":"Other","al":{"picUrl":"https://replacement"}},{"id":2,"name":"Other","al":{"picUrl":"https://two"}}]}"#.to_vec())
            }
        }).unwrap();
        assert_eq!(calls, 2);
        assert_eq!(
            songs
                .iter()
                .map(|song| song.cover_url.as_str())
                .collect::<Vec<_>>(),
            vec!["https://existing", "https://two", "https://three"]
        );
        assert_eq!(songs[1].name, "Two");
    }

    #[test]
    fn search_does_not_request_detail_when_covers_are_present() {
        let mut calls = 0;
        let songs = search_with("test", "", |_| {
            calls += 1;
            Ok(br#"{"code":200,"result":{"songs":[{"id":1,"name":"One","al":{"picUrl":"https://existing"}}]}}"#.to_vec())
        }).unwrap();
        assert_eq!(calls, 1);
        assert_eq!(songs[0].cover_url, "https://existing");
    }

    #[test]
    fn search_keeps_results_when_detail_fails() {
        for detail in [
            Err("network".to_string()),
            Ok(b"invalid".to_vec()),
            Ok(br#"{"code":500,"songs":[]}"#.to_vec()),
        ] {
            let mut calls = 0;
            let songs = search_with("test", "", |_| {
                calls += 1;
                if calls == 1 {
                    Ok(br#"{"code":200,"result":{"songs":[{"id":1,"name":"One"}]}}"#.to_vec())
                } else {
                    detail.clone()
                }
            })
            .unwrap();
            assert_eq!(calls, 2);
            assert_eq!(songs[0].name, "One");
            assert!(songs[0].cover_url.is_empty());
        }
    }

    #[test]
    fn audio_validation_rejects_non_mp3_and_accepts_frames() {
        let directory = artwork::unique_output_directory("netease-audio-test");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("audio");
        fs::write(&path, b"<html>error</html>").unwrap();
        assert!(validate_audio(&path).is_err());
        let mut frame = vec![0; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
        fs::write(&path, frame.repeat(2)).unwrap();
        assert!(validate_audio(&path).is_ok());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn retries_are_bounded_and_return_eventual_success() {
        let mut attempts = 0;
        let value = retry("test", || {
            attempts += 1;
            if attempts < 3 {
                Err("bad response".to_string())
            } else {
                Ok(42)
            }
        })
        .unwrap();
        assert_eq!(value, 42);
        assert_eq!(attempts, 3);
        let mut attempts = 0;
        assert!(
            retry::<()>("test", || {
                attempts += 1;
                Err("failure".into())
            })
            .is_err()
        );
        assert_eq!(attempts, 3);
    }

    #[test]
    fn lyrics_responses_validate_errors_and_allow_missing_lyrics() {
        assert!(
            validate_lyrics(br#"{"code":200,"nolyric":true}"#)
                .unwrap()
                .is_none()
        );
        assert!(
            validate_lyrics(br#"{"code":200,"lrc":{"lyric":""}}"#)
                .unwrap()
                .is_none()
        );
        assert!(
            validate_lyrics(br#"{"code":200,"lrc":{"lyric":"[00:01.00]hello"}}"#)
                .unwrap()
                .is_some()
        );
        for bytes in [
            &b""[..],
            &b"not json"[..],
            &br#"{"code":500}"#[..],
            &br#"{"code":200,"lrc":42}"#[..],
            &br#"{}"#[..],
        ] {
            assert!(validate_lyrics(bytes).is_err());
        }
    }

    #[test]
    fn prepare_retries_address_and_cover_lookup_and_reports_optional_failures() {
        let directory = artwork::unique_output_directory("netease-prepare-test");
        let song = CloudSong {
            id: 7,
            name: "Track".into(),
            artists: vec![],
            album: String::new(),
            duration_ms: 0,
            cover_url: String::new(),
        };
        let mut addresses = 0;
        let mut details = 0;
        let mut lyrics = 0;
        let mut covers = 0;
        let prepared = prepare_with(&song, "", AUDIO_BITRATE_LOW, &directory, |request| {
            if request.url.ends_with("/song/enhance/player/url") {
                addresses += 1;
                if addresses == 1 { return Err("HTTP 503".into()); }
                if addresses == 2 { return Ok(b"invalid".to_vec()); }
                Ok(br#"{"code":200,"data":[{"url":"https://audio"}]}"#.to_vec())
            } else if request.url.ends_with("/v3/song/detail") {
                details += 1;
                Ok(br#"{"code":200,"songs":[{"id":7,"name":"Track","al":{"picUrl":"https://cover"}}]}"#.to_vec())
            } else {
                lyrics += 1;
                Err("HTTP 500".into())
            }
        }, |url, path, _| {
            if url == "https://cover" { covers += 1; return Err("HTTP 404".into()); }
            let mut frame = vec![0; 417];
            frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
            fs::write(path, frame.repeat(2)).unwrap();
            Ok(834)
        }).unwrap();
        assert_eq!((addresses, details, covers, lyrics), (3, 1, 3, 3));
        assert_eq!(prepared.song.cover_url, "https://cover");
        assert_eq!(prepared.warnings.len(), 2);
        assert!(prepared.warnings[0].contains("HTTP 404"));
        assert!(prepared.warnings[1].contains("HTTP 500"));
        assert_eq!(prepared.temp_directory, directory);
        assert!(directory.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn prepare_cleans_directory_on_failure_and_retries_address() {
        let directory = artwork::unique_output_directory("netease-cleanup-test");
        let song = CloudSong {
            id: 7,
            name: "Track".into(),
            artists: vec![],
            album: String::new(),
            duration_ms: 0,
            cover_url: String::new(),
        };
        let mut attempts = 0;
        let error = match prepare_with(
            &song,
            "",
            AUDIO_BITRATE_LOW,
            &directory,
            |_| {
                attempts += 1;
                fs::write(directory.join("partial"), b"partial").unwrap();
                Ok(b"invalid".to_vec())
            },
            |_, _, _| panic!("download must not run"),
        ) {
            Ok(_) => panic!("expected failure"),
            Err(error) => error,
        };
        assert_eq!(attempts, 3);
        assert!(error.contains("歌曲地址"));
        assert!(!directory.exists());
    }

    #[test]
    fn prepare_no_lyrics_is_not_a_warning() {
        let directory = artwork::unique_output_directory("netease-no-lyrics-test");
        let song = CloudSong {
            id: 7,
            name: "Track".into(),
            artists: vec![],
            album: String::new(),
            duration_ms: 0,
            cover_url: String::new(),
        };
        let prepared = prepare_with(
            &song,
            "",
            AUDIO_BITRATE_LOW,
            &directory,
            |request| {
                if request.url.ends_with("/song/enhance/player/url") {
                    Ok(br#"{"code":200,"data":[{"url":"https://audio"}]}"#.to_vec())
                } else if request.url.ends_with("/v3/song/detail") {
                    Ok(br#"{"code":200,"songs":[]}"#.to_vec())
                } else {
                    Ok(br#"{"code":200,"nolyric":true}"#.to_vec())
                }
            },
            |_, path, _| {
                let mut frame = vec![0; 417];
                frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
                fs::write(path, frame.repeat(2)).unwrap();
                Ok(834)
            },
        )
        .unwrap();
        assert_eq!(prepared.warnings.len(), 1);
        assert!(prepared.warnings[0].contains("无法取得封面"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn prepare_warns_when_detail_omits_cover_address() {
        for detail in [
            br#"{"code":200,"songs":[{"id":7,"name":"Track","al":{}}]}"#.as_slice(),
            br#"{"code":200,"songs":[{"id":7,"name":"Track","al":{"picUrl":"  "}}]}"#.as_slice(),
            br#"{"code":200,"songs":[{"id":8,"name":"Other","al":{"picUrl":"https://cover"}}]}"#
                .as_slice(),
        ] {
            let directory = artwork::unique_output_directory("netease-missing-cover-test");
            let song = CloudSong {
                id: 7,
                name: "Track".into(),
                artists: vec![],
                album: String::new(),
                duration_ms: 0,
                cover_url: String::new(),
            };
            let prepared = prepare_with(
                &song,
                "",
                AUDIO_BITRATE_LOW,
                &directory,
                |request| {
                    if request.url.ends_with("/song/enhance/player/url") {
                        Ok(br#"{"code":200,"data":[{"url":"https://audio"}]}"#.to_vec())
                    } else if request.url.ends_with("/v3/song/detail") {
                        Ok(detail.to_vec())
                    } else {
                        Ok(br#"{"code":200,"nolyric":true}"#.to_vec())
                    }
                },
                |url, path, _| {
                    assert_eq!(url, "https://audio");
                    let mut frame = vec![0; 417];
                    frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
                    fs::write(path, frame.repeat(2)).unwrap();
                    Ok(834)
                },
            )
            .unwrap();
            assert_eq!(prepared.warnings.len(), 1);
            assert!(prepared.warnings[0].contains("无法取得封面"));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn eapi_encrypts_search_and_attaches_cookie() {
        let request = eapi(
            "/api/search/get",
            json!({ "s": "测试", "type": 1 }),
            "MUSIC_U=abc",
        );
        assert_eq!(
            request.url,
            "https://interfacepc.music.163.com/eapi/search/get"
        );
        let body = String::from_utf8(request.body).unwrap();
        assert!(body.starts_with("params="));
        assert!(!body.contains("测试"));
        assert!(request.headers.iter().any(|(name, value)| {
            name == "Cookie" && value.contains("MUSIC_U=abc") && value.contains("deviceId=")
        }));
    }

    #[test]
    fn qr_requests_match_go_musicfox_weapi_shape() {
        let request = weapi(
            "/api/login/qrcode/unikey",
            json!({ "type": 1, "noCheckToken": true }),
            "sDeviceId=ABCDEF",
        )
        .unwrap();
        assert_eq!(
            request.url,
            "https://music.163.com/weapi/login/qrcode/unikey"
        );
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| { name == "User-Agent" && value == WEB_USER_AGENT })
        );
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| { name == "Referer" && value == "https://music.163.com" })
        );
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| { name == "Cookie" && value == "sDeviceId=ABCDEF" })
        );
        let body = String::from_utf8(request.body).unwrap();
        let enc_sec_key = body.split("encSecKey=").nth(1).unwrap();
        assert!(body.starts_with("params="));
        assert_eq!(enc_sec_key.len(), 256);
        assert!(enc_sec_key.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn weapi_aes_matches_openssl_vector() {
        assert_eq!(
            aes_cbc_base64(b"hello", WEAPI_NONCE).unwrap(),
            "+J9Q3vLzLGFuqlWFQh3T3A=="
        );
    }

    #[test]
    fn generated_device_identity_matches_upstream_shape() {
        let id = device_id();
        assert_eq!(id.len(), 52);
        assert!(
            id.bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())
        );
    }

    #[test]
    fn web_qr_chain_id_shape_is_url_safe() {
        let timestamp = 1_700_000_000_123u128;
        let chain_id = format!("v1_ABCDEF_web_login_{timestamp}");
        assert_eq!(chain_id, "v1_ABCDEF_web_login_1700000000123");
    }

    #[test]
    fn qr_cookie_jar_replaces_values_and_ignores_attributes() {
        let mut cookies = vec!["sDeviceId=ABC".to_string(), "MUSIC_A=old".to_string()];
        merge_cookies(
            &mut cookies,
            split_cookie_header("MUSIC_A=new; Path=/; HttpOnly; __csrf=token"),
        );
        assert!(cookies.contains(&"sDeviceId=ABC".to_string()));
        assert!(cookies.contains(&"MUSIC_A=new".to_string()));
        assert!(cookies.contains(&"__csrf=token".to_string()));
        assert!(!cookies.iter().any(|item| item.starts_with("Path=")));
    }

    #[test]
    fn playlist_shapes_map_user_and_tracks() {
        let playlists: PlaylistEnvelope = serde_json::from_str(
            r#"{"code":200,"playlist":[{"id":9,"name":"Favorites","trackCount":2,"coverImgUrl":"https://cover","creator":{"nickname":"User"}}]}"#,
        )
        .unwrap();
        assert_eq!(playlists.playlist[0].id, 9);
        assert_eq!(playlists.playlist[0].track_count, 2);
        let detail: PlaylistDetailEnvelope = serde_json::from_str(
            r#"{"code":200,"playlist":{"name":"Favorites","tracks":[{"id":7,"name":"Track","ar":[{"name":"Artist"}],"al":{"id":8,"name":"Album"},"dt":1234}]}}"#,
        )
        .unwrap();
        let song = cloud_song(detail.playlist.tracks.into_iter().next().unwrap());
        assert_eq!(song.id, 7);
        assert_eq!(song.artists, vec!["Artist"]);
        assert_eq!(song.duration_ms, 1234);
    }

    #[test]
    fn playlist_tracks_accept_null_optional_metadata() {
        let detail: PlaylistDetailEnvelope = serde_json::from_str(
            r#"{"code":200,"playlist":{"name":"Favorites","tracks":[{"id":7,"name":null,"ar":[{"name":null}],"al":{"id":8,"name":null,"picUrl":null},"dt":1234}]}}"#,
        )
        .unwrap();
        let song = cloud_song(detail.playlist.tracks.into_iter().next().unwrap());
        assert_eq!(song.name, "");
        assert_eq!(song.artists, vec![""]);
        assert_eq!(song.album, "");
        assert_eq!(song.cover_url, "");
    }

    #[test]
    fn search_shape_maps_cover_and_metadata() {
        let value: SearchEnvelope = serde_json::from_str(
            r#"{"code":200,"result":{"songs":[{"id":7,"name":"Track","ar":[{"name":"Artist"}],"al":{"id":8,"name":"Album","picUrl":"https://cover"},"dt":1234}]}}"#,
        )
        .unwrap();
        let song = value.result.songs.into_iter().next().unwrap();
        assert_eq!(song.id, 7);
        assert_eq!(song.al.pic_url, "https://cover");
        assert_eq!(song.dt, 1234);
    }
}
