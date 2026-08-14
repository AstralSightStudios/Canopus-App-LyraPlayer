use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use aes::Aes128;
use cipher::{BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use md5::{Digest, Md5};
use serde::Deserialize;
use serde_json::{Value, json};
use waki::{Client, Method};

use crate::{import::ImportAsset, state::CloudSong};

const API_BASE: &str = "https://interfacepc.music.163.com";
const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";
const EAPI_DELIMITER: &str = "-36cd479b6b5-";
const USER_AGENT: &str = "NeteaseMusic 9.0.90/5038 (iPhone; iOS 16.2; zh_CN)";
const MAX_AUDIO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COVER_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
struct ApiRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
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
struct SongWire {
    id: u64,
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
    name: String,
}

#[derive(Default, Deserialize)]
struct AlbumWire {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default, alias = "picUrl")]
    pic_url: String,
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
    let value: SearchEnvelope = serde_json::from_slice(&bytes)
        .map_err(|error| format!("无法解析搜索结果：{error}"))?;
    if value.code != 200 {
        return Err(format!("网易云搜索失败：{}", value.code));
    }
    Ok(value
        .result
        .songs
        .into_iter()
        .map(|song| CloudSong {
            id: song.id,
            name: song.name,
            artists: song.ar.into_iter().map(|artist| artist.name).collect(),
            album: song.al.name,
            album_id: song.al.id,
            duration_ms: song.dt,
            cover_url: song.al.pic_url,
        })
        .collect())
}

pub fn begin_qr_login() -> Result<(String, String), String> {
    let bytes = perform(eapi(
        "/api/login/qrcode/unikey",
        json!({ "type": 3 }),
        "",
    ))?;
    let value: QrKeyEnvelope = serde_json::from_slice(&bytes)
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
    let url = format!("https://music.163.com/login?codekey={}", url_encode(&key));
    Ok((key, url))
}

pub fn poll_qr_login(key: &str) -> Result<Option<String>, String> {
    let bytes = perform(eapi(
        "/api/login/qrcode/client/login",
        json!({ "key": key, "type": 3 }),
        "",
    ))?;
    let value: QrCheckEnvelope = serde_json::from_slice(&bytes)
        .map_err(|error| format!("无法解析扫码状态：{error}"))?;
    match value.code {
        800 => Err("二维码已过期，请重新生成".to_string()),
        801 | 802 => Ok(None),
        803 => value
            .cookie
            .filter(|cookie| !cookie.is_empty())
            .map(Some)
            .ok_or_else(|| "登录成功但未返回 Cookie".to_string()),
        code => Err(format!("扫码登录失败：{code}")),
    }
}

pub fn prepare(song: &CloudSong, cookie: &str) -> Result<PreparedCloud, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = PathBuf::from("media").join(format!("netease-{}-{nonce}", song.id));
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建下载目录：{error}"))?;

    let song_url_bytes = perform(eapi(
        "/api/song/enhance/player/url",
        json!({ "ids": format!("[\"{}\"]", song.id), "br": 320000 }),
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

    if !song.cover_url.is_empty() {
        let cover_path = directory.join("cover.jpg");
        match download(&song.cover_url, &cover_path, MAX_COVER_BYTES) {
            Ok(size) => assets.push(ImportAsset::cover(path_text(&cover_path)?, size, "jpg")),
            Err(error) => tracing::warn!("cover download skipped: {error}"),
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
    let bytes = response
        .body()
        .map_err(|error| format!("读取网易云响应失败：{error}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("网易云 HTTP {status}"));
    }
    Ok(bytes)
}

fn download(url: &str, path: &PathBuf, limit: u64) -> Result<u64, String> {
    let client = Client::new();
    let response = client
        .request(Method::Get, url)
        .header(waki::header::USER_AGENT, USER_AGENT)
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

fn eapi(path: &str, mut data: Value, cookie: &str) -> ApiRequest {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let header = json!({
        "osver": "16.2",
        "deviceId": "astrobox-lyra-import",
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
    let mut headers = vec![
        ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
        ("User-Agent".into(), USER_AGENT.into()),
    ];
    if !cookie.trim().is_empty() {
        headers.push(("Cookie".into(), cookie.trim().into()));
    }
    ApiRequest {
        url: format!("{API_BASE}/eapi/{}", path.trim_start_matches("/api/")),
        headers,
        body: format!("params={params}").into_bytes(),
    }
}

fn path_text(path: &PathBuf) -> Result<String, String> {
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
        assert_eq!(request.url, "https://interfacepc.music.163.com/eapi/search/get");
        let body = String::from_utf8(request.body).unwrap();
        assert!(body.starts_with("params="));
        assert!(!body.contains("测试"));
        assert!(request.headers.iter().any(|(name, value)| name == "Cookie" && value == "MUSIC_U=abc"));
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
