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
    #[serde(default, alias = "coverImgUrl")]
    cover_url: String,
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
    #[serde(default)]
    id: u64,
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
    pub assets: Vec<ImportAsset>,
}

pub fn search(query: &str, cookie: &str) -> Result<Vec<CloudSong>, String> {
    let request = eapi(
        "/api/search/get",
        json!({ "s": query, "type": 1, "limit": 20, "offset": 0 }),
        cookie,
    );
    let bytes = perform(request)?;
    let value: SearchEnvelope =
        serde_json::from_slice(&bytes).map_err(|error| format!("无法解析搜索结果：{error}"))?;
    if value.code != 200 {
        return Err(format!("网易云搜索失败：{}", value.code));
    }
    Ok(value.result.songs.into_iter().map(cloud_song).collect())
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
            cover_url: playlist.cover_url,
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
        album_id: song.al.id,
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
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建下载目录：{error}"))?;

    let song_url_bytes = perform(eapi(
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
    if url.is_empty() {
        return Err("歌曲因版权或区域限制无法下载".to_string());
    }

    let audio_path = directory.join("audio.mp3");
    let audio_size = download(&url, &audio_path, MAX_AUDIO_BYTES)?;
    let mut assets = vec![ImportAsset::audio(path_text(&audio_path)?, audio_size)];

    if song.cover_url.is_empty() {
        tracing::warn!(song_id = song.id, "NetEase song has no album cover URL");
    } else {
        let source_path = directory.join("cover-source.jpg");
        match download(&song.cover_url, &source_path, MAX_COVER_BYTES) {
            Ok(size) => match artwork::prepare(&source_path, &directory) {
                Ok(prepared) => {
                    tracing::info!(
                        song_id = song.id,
                        source_bytes = size,
                        cover_bytes = prepared.cover_size,
                        background_bytes = prepared.background_size.unwrap_or(0),
                        "NetEase artwork prepared"
                    );
                    assets.push(ImportAsset::cover_bin(
                        prepared.cover_path,
                        prepared.cover_size,
                    ));
                    if let (Some(path), Some(size)) =
                        (prepared.background_path, prepared.background_size)
                    {
                        assets.push(ImportAsset::background_bin(path, size));
                    }
                }
                Err(error) => tracing::warn!(
                    song_id = song.id,
                    error = %error,
                    "NetEase artwork processing skipped"
                ),
            },
            Err(error) => tracing::warn!(
                song_id = song.id,
                error = %error,
                "NetEase artwork download skipped"
            ),
        }
    }

    let lyrics = perform(eapi(
        "/api/song/lyric/v1",
        json!({ "id": song.id, "cp": false, "tv": 0, "lv": 0, "rv": 0, "kv": 0, "yv": 0, "ytv": 0, "yrv": 0 }),
        cookie,
    ))?;
    if lyrics.len() <= 2 * 1024 * 1024 {
        let lyrics_path = directory.join("lyrics.json");
        fs::write(&lyrics_path, &lyrics).map_err(|error| format!("无法保存歌词：{error}"))?;
        assets.push(ImportAsset::lyrics(
            path_text(&lyrics_path)?,
            lyrics.len() as u64,
            "json",
        ));
    }

    Ok(PreparedCloud {
        song: song.clone(),
        assets,
    })
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
