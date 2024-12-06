//
// based on https://github.com/gmg137/netease-cloud-music-api
//
mod config;
mod encrypt;
pub(crate) mod model;

use crate::config::*;
use crate::encrypt::Crypto;
pub use crate::model::*;
use anyhow::{anyhow, Result};
use cookie_store;
use cookie_store::CookieStore;
pub use isahc::cookies::{CookieBuilder, CookieJar};
use isahc::{prelude::*, *};
use lazy_static::lazy_static;
use log::{error, trace};
use regex::Regex;
use std::cell::RefCell;
use std::sync::Arc;
use std::{collections::HashMap, fs, io, path::PathBuf, time::Duration};
use tokio::sync::Mutex;
use urlqstring::QueryParams;
use clap::{Command, Arg};

lazy_static! {
    static ref _CSRF: Regex = Regex::new(r"_csrf=(?P<csrf>[^(;|$)]+)").unwrap();
}

#[derive(Clone)]
pub struct NcmApi {
    client: HttpClient,
    csrf: Arc<Mutex<RefCell<String>>>,

    cookie_path: PathBuf,
    lyrics_path: PathBuf,
    cache_path: PathBuf,

    is_login: bool,
    login_info: Option<LoginInfo>,
    rate: u32,

    user_favorite_songlist_name: Option<String>,
    user_favorite_songlist: Option<Vec<SongInfo>>,
}

#[allow(unused)]
enum CryptoApi {
    Weapi,
    LinuxApi,
    Eapi,
}

impl NcmApi {
    pub fn new(cookie_path: PathBuf, lyrics_path: PathBuf, cache_path: PathBuf) -> Self {
        let client = HttpClient::builder()
            .timeout(Duration::from_secs(TIMEOUT))
            .max_connections(DEFAULT_MAX_CONNECTIONS)
            .cookies()
            .build()
            .expect("初始化网络请求失败!");
        Self {
            client,
            csrf: Arc::new(Mutex::new(RefCell::new(String::new()))),
            cookie_path,
            lyrics_path,
            cache_path,
            is_login: false,
            login_info: None,
            rate: DEFAULT_RATE,
            user_favorite_songlist_name: None,
            user_favorite_songlist: None,
        }
    }
}

/// cookie 相关
impl NcmApi {
    pub fn from_cookie_jar(
        cookie_path: PathBuf,
        lyrics_path: PathBuf,
        cache_path: PathBuf,
    ) -> Self {
        if let Some(cookie_jar) = Self::load_cookie_jar_from_file(cookie_path.clone()) {
            Self {
                client: Self::create_client_from_cookie_jar(cookie_jar),
                csrf: Arc::new(Mutex::new(RefCell::new(String::new()))),
                cookie_path,
                lyrics_path,
                cache_path,
                is_login: false,
                login_info: None,
                rate: DEFAULT_RATE,
                user_favorite_songlist_name: None,
                user_favorite_songlist: None,
            }
        } else {
            Self::new(cookie_path, lyrics_path, cache_path)
        }
    }

    fn create_client_from_cookie_jar(cookie_jar: CookieJar) -> HttpClient {
        HttpClient::builder()
            .timeout(Duration::from_secs(TIMEOUT))
            .max_connections(DEFAULT_MAX_CONNECTIONS)
            .cookies()
            .cookie_jar(cookie_jar)
            .build()
            .expect("初始化网络请求失败!")
    }

    fn load_cookie_jar_from_file(cookie_store_path: PathBuf) -> Option<CookieJar> {
        use cookie_store::serde;

        match fs::File::open(cookie_store_path) {
            Ok(file) => match serde::json::load(io::BufReader::new(file)) {
                Ok(cookie_store) => {
                    let cookie_jar = CookieJar::default();

                    for base_url in BASE_URL_LIST {
                        for c in cookie_store.matches(&base_url.parse().unwrap()) {
                            let cookie = CookieBuilder::new(c.name(), c.value())
                                .domain("music.163.com")
                                .path(c.path().unwrap_or("/"))
                                .build()
                                .unwrap();
                            cookie_jar.set(cookie, &base_url.parse().unwrap()).unwrap();
                        }
                    }

                    return Some(cookie_jar);
                }
                Err(err) => error!("{:?}", err),
            },
            Err(err) => match err.kind() {
                io::ErrorKind::NotFound => (),
                other => error!("{:?}", other),
            },
        };

        None
    }

    pub fn cookie_jar(&self) -> Option<&CookieJar> {
        self.client.cookie_jar()
    }

    pub fn store_cookie(&self) {
        use cookie_store::serde;

        if let Some(cookie_jar) = self.cookie_jar() {
            match fs::File::create(&self.cookie_path) {
                Ok(mut file) => {
                    let mut cookie_store = CookieStore::default();

                    for base_url in BASE_URL_LIST {
                        let url = &base_url.parse().unwrap();
                        let uri = &base_url.parse().unwrap();

                        for c in cookie_jar.get_for_uri(url) {
                            let cookie = cookie_store::Cookie::parse(
                                format!(
                                    "{}={}; Path={}; Domain=music.163.com; Max-Age=31536000",
                                    c.name(),
                                    c.value(),
                                    url.path()
                                ),
                                uri,
                            )
                            .unwrap();
                            cookie_store.insert(cookie, uri).unwrap();
                        }
                    }

                    serde::json::save(&cookie_store, &mut file).unwrap();
                }
                Err(err) => error!("{:?}", err),
            }
        }
    }
}

/// 登录相关
impl NcmApi {
    /// 创建登陆二维码链接
    /// 返回(qr_url, unikey)
    pub async fn login_qr_create(&self) -> Result<(String, String)> {
        let path = "/weapi/login/qrcode/unikey";
        let mut params = HashMap::new();
        params.insert("type", "1");
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        let unikey = to_unikey(result)?;
        Ok((
            format!("https://music.163.com/login?codekey={}", &unikey),
            unikey,
        ))
    }

    /// 检查登陆二维码
    /// key: 由 login_qr_create 生成的 unikey
    pub async fn login_qr_check(&self, key: String) -> Result<Msg> {
        let path = "/weapi/login/qrcode/client/login";
        let mut params = HashMap::new();
        params.insert("type", "1");
        params.insert("key", &key);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_message(result)
    }

    /// 登录状态
    pub async fn login_status(&self) -> Result<LoginInfo> {
        let path = "/api/nuser/account/get";
        let result = self
            .request(
                Method::Post,
                path,
                HashMap::new(),
                CryptoApi::Weapi,
                "",
                true,
            )
            .await?;
        to_login_info(result)
    }

    /// 使用 cookie 登录时尝试检查登录状态
    pub async fn check_cookie_login(&mut self) -> Result<bool> {
        match self.login_status().await {
            Ok(login_info) => {
                self.login_info = Some(login_info);
                self.is_login = true;

                self.get_user_favorite_songlist().await?;

                Ok(true)
            }
            Err(_err) => Ok(false),
        }
    }

    /// 新账号已验证登录后，初始化
    pub async fn init_after_new_login(&mut self) -> Result<()> {
        self.store_cookie();

        self.login_info = Some(self.login_status().await?);
        self.is_login = true;

        self.get_user_favorite_songlist().await?;

        Ok(())
    }

    /// 退出
    pub async fn logout(&mut self) {
        // let path = "https://music.163.com/weapi/logout";
        // self.request(
        //     Method::Post,
        //     path,
        //     HashMap::new(),
        //     CryptoApi::Weapi,
        //     "pc",
        //     true,
        // ).await.expect("failed to logout");

        self.is_login = false;
    }
}

/// getter
impl NcmApi {
    /// 是否登录
    pub fn is_login(&self) -> bool {
        self.is_login
    }

    /// 登录用户信息
    pub fn login_info(&self) -> Option<LoginInfo> {
        self.login_info.clone()
    }

    /// “我喜欢的音乐”歌单
    pub fn user_favorite_songlist(&self) -> (Option<String>, Option<Vec<SongInfo>>) {
        (
            self.user_favorite_songlist_name.clone(),
            self.user_favorite_songlist.clone(),
        )
    }
}

/// 音乐播放 API
impl NcmApi {
    /// 获取音乐（单曲） url
    /// 使用 Eapi 获取音乐
    pub async fn get_song_url(&self, id: u64) -> Result<String> {
        let song_urls = self.get_song_urls(&[id]).await?;

        if !song_urls.is_empty() {
            Ok(song_urls[0].url.clone())
        } else {
            Err(anyhow!("failed to get song url"))
        }
    }

    /// 歌曲 URL
    /// ids: 歌曲列表
    /// br: 歌曲码率
    pub async fn get_song_urls(&self, ids: &[u64]) -> Result<Vec<SongUrl>> {
        let br = self.rate.clone().to_string();
        // 使用 Eapi 获取音乐
        let path = "https://interface3.music.163.com/eapi/song/enhance/player/url";
        let mut params = HashMap::new();
        let ids = serde_json::to_string(ids)?;
        params.insert("ids", ids.as_str());
        params.insert("br", br.as_str());
        let result = self
            .request(Method::Post, path, params, CryptoApi::Eapi, "", true)
            .await?;
        to_song_url(result)
    }

    /// 获取歌词
    pub async fn song_lyric(&self, si: SongInfo) -> Result<Vec<(u64, (String, Option<String>))>> {
        // 歌词文件位置
        let mut lyric_path = self.lyrics_path.clone();
        lyric_path.push(format!(
            "{}-{}-{}.lrc",
            si.name.replace('/', "／"),
            si.singer,
            si.album
        ));
        // 翻译歌词文件位置
        let mut translation_lyric_path = self.cache_path.clone();
        translation_lyric_path.push(format!("{}.tlrc", si.id));

        // 替换歌词时间
        let timestamp_re = Regex::new(r"\[\d+:\d+.\d+]")?;
        // 修正不正常的时间戳 [00:11:22]
        let re_abnormal_ts = Regex::new(r"^\[(\d+):(\d+):(\d+)]")?;
        // 补全7位时间戳
        let re_7bit_timestamp = Regex::new(r"\[(\d+):(\d+)]")?;

        if !lyric_path.exists() {
            // 创建歌词文件，访问网易云接口获取
            if let Ok(lyric_origin) = self.get_song_lyric(si.id).await {
                trace!("歌词: {:?}", lyric_origin);

                let lyric_fixed = Lyrics {
                    lyric: lyric_origin.lyric
                        .into_iter()
                        .map(|x| {
                            let mut fixed = re_abnormal_ts.replace_all(&x, "[$1:$2.$3]").to_string();
                            fixed = re_7bit_timestamp.replace_all(&fixed, "[$1:$2.000]").to_string();
                            fixed
                        })
                        .collect(),
                    tlyric: lyric_origin.tlyric
                        .into_iter()
                        .map(|x| {
                            let mut fixed = re_abnormal_ts.replace_all(&x, "[$1:$2.$3]").to_string();
                            fixed = re_7bit_timestamp.replace_all(&fixed, "[$1:$2.000]").to_string();
                            fixed
                        })
                        .collect(),
                };

                // 修正并保存歌词文件
                let lyric = lyric_fixed
                    .lyric
                    .clone()
                    .join("\n");
                fs::write(&lyric_path, lyric)?;
                if !lyric_fixed.tlyric.is_empty() {
                    // 保存翻译歌词文件
                    let tlyric = lyric_fixed
                        .tlyric
                        .clone()
                        .join("\n");
                    fs::write(&translation_lyric_path, tlyric)?;
                }

                // 编码
                let lyrics_with_timestamp = self.encode_lyric(&timestamp_re, &lyric_fixed);

                // 组织歌词+翻译
                Ok(lyrics_with_timestamp)
            } else {
                anyhow::bail!("No lyrics found!")
            }
        } else {
            // 歌词文件已存在缓存
            let lyric_file = fs::read_to_string(&lyric_path)?;
            let lyric: Vec<String> = lyric_file
                .split('\n')
                .collect::<Vec<&str>>()
                .iter()
                .map(|s| s.to_string())
                .collect();
            let mut tlyric = vec![];
            if translation_lyric_path.exists() {
                let tlyric_file = fs::read_to_string(&translation_lyric_path)?;
                tlyric = tlyric_file
                    .split('\n')
                    .collect::<Vec<&str>>()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            }

            // 编码歌词和翻译
            let lyrics = Lyrics { lyric, tlyric };
            trace!("read lyric: {:?}", lyrics);
            let lyrics_with_stamp = self.encode_lyric(&timestamp_re, &lyrics);

            // 组织歌词+翻译
            Ok(lyrics_with_stamp)
        }
    }

    /// 编码歌词和翻译
    fn encode_lyric(
        &self,
        timestamp_re: &Regex,
        lyrics: &Lyrics,
    ) -> Vec<(u64, (String, Option<String>))> {
        let mut lyrics_with_timestamp: Vec<(u64, (String, Option<String>))> = Vec::new();

        // 以倒序遍历歌词文件和翻译文件，避免 [00:00.000] 同时有多行歌前信息（作词/作曲/编曲/...）和第一句歌词造成的翻译匹配错误
        let lyric_rev_iter = lyrics.lyric.iter().rev();
        // 翻译部分的功能较复杂，不用迭代器实现
        let t_lyric = &lyrics.tlyric;
        let mut t_lyric_rev_pointer: usize;
        let mut have_t_lyric = !t_lyric.is_empty();
        t_lyric_rev_pointer = if have_t_lyric {
            lyrics.tlyric.len() - 1
        } else {
            0
        };
        //
        for lyric in lyric_rev_iter {
            if lyric.len() >= 10 && timestamp_re.is_match(lyric) {
                let time = (lyric[1..3].parse::<u64>().unwrap() * 60
                    + lyric[4..6].parse::<u64>().unwrap())
                    * 1000
                    + lyric[7..9].parse::<u64>().unwrap_or(0) * 10;

                let mut lyric_line = timestamp_re.replace_all(lyric, "").to_string();
                lyric_line = lyric_line.trim_end_matches("\t").to_string(); // 部分句尾存在\t字符干扰渲染
                lyric_line.push('\n');

                // 翻译位置先填 None
                lyrics_with_timestamp.push((time, (lyric_line, None)));
            }
            if have_t_lyric {
                if let Some(t_lyric) = t_lyric.get(t_lyric_rev_pointer) {
                    if t_lyric.len() >= 10
                        && lyric.len() >= 10
                        && t_lyric.starts_with(&lyric[0..10])
                    {
                        let mut t_lyric_line = timestamp_re.replace_all(t_lyric, "").to_string();
                        t_lyric_line = t_lyric_line.trim_end_matches("\t").to_string(); // 部分句尾存在\t字符干扰渲染
                        t_lyric_line.push('\n');

                        // 更新对应的翻译
                        if let Some(last) = lyrics_with_timestamp.as_mut_slice().last_mut() {
                            *last = (last.0.clone(), (last.1 .0.clone(), Some(t_lyric_line)));
                        }

                        // 只有当翻译匹配到时间戳相同的歌词时指针才移动
                        if t_lyric_rev_pointer > 0 {
                            t_lyric_rev_pointer -= 1;
                        } else {
                            // t_lyric_rev_pointer == 0
                            // 翻译已经遍历到头
                            have_t_lyric = false;
                        }
                    }
                }
            }
        }

        // 倒转恢复顺序
        lyrics_with_timestamp.reverse();

        lyrics_with_timestamp
    }

    /// 查询歌词
    /// music_id: 歌曲id
    pub async fn get_song_lyric(&self, music_id: u64) -> Result<Lyrics> {
        let csrf_token = self.csrf.lock().await.borrow().to_owned();
        let path = "/weapi/song/lyric";
        let mut params = HashMap::new();
        let id = music_id.to_string();
        params.insert("id", &id[..]);
        params.insert("lv", "-1");
        params.insert("tv", "-1");
        params.insert("csrf_token", &csrf_token);

        // let result = self
        //     .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
        //     .await?;

        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;

        to_lyric(result)
    }
}

/// 音乐信息获取 API
impl NcmApi {
    pub async fn get_user_favorite_songlist(&mut self) -> Result<()> {
        // 解析命令行参数
        let matches = Command::new("xxx")
            .arg(Arg::new("playList")
                .long("playList")
                .value_parser(clap::value_parser!(i32))  // 使用 value_parser 来解析 i32 类型
                .required(true)
                .help("The play list identifier"))
            .get_matches();

        // 获取传入的 playList 参数并转换为 u16
        let play_list = *matches.get_one::<i32>("playList").unwrap() as u16;

        match &self.login_info {
            Some(login_info) => {
                let user_id = login_info.uid.clone();  // 获取登录用户的 id

                // 调用 user_song_list 方法时使用 u16 类型的参数
                match self.user_song_list(user_id, play_list, 1).await {
                    Ok(user_songlists) => {
                        if !user_songlists.is_empty() {
                            self.user_favorite_songlist_name = Some(user_songlists[0].name.clone());
                            self.user_favorite_songlist =
                                Some(self.song_list_detail(user_songlists[0].id).await?.songs);

                            Ok(())
                        } else {
                            Err(anyhow!("user has no songlist."))
                        }
                    }
                    Err(err) => Err(err),
                }
            }
            None => Err(anyhow!("you have to login first.")),
        }
    }

    /// 用户歌单
    /// uid: 用户id
    /// offset: 列表起点号
    /// limit: 列表长度
    pub async fn user_song_list(&self, uid: u64, offset: u16, limit: u16) -> Result<Vec<SongList>> {
        let path = "/weapi/user/playlist";
        let mut params = HashMap::new();
        let uid = uid.to_string();
        let offset = offset.to_string();
        let limit = limit.to_string();
        params.insert("uid", uid.as_str());
        params.insert("offset", offset.as_str());
        params.insert("limit", limit.as_str());
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_list(result, Parse::Usl)
    }
}

/// 待重构
impl NcmApi {
    /// 用户音乐id列表
    /// uid: 用户id
    pub async fn user_song_id_list(&self, uid: u64) -> Result<Vec<u64>> {
        let path = "/weapi/song/like/get";
        let mut params = HashMap::new();
        let uid = uid.to_string();
        params.insert("uid", uid.as_str());
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_id_list(result)
    }

    /// 用户收藏专辑列表
    /// offset: 列表起点号
    /// limit: 列表长度
    pub async fn album_sublist(&self, offset: u16, limit: u16) -> Result<Vec<SongList>> {
        let path = "/weapi/album/sublist";
        let mut params = HashMap::new();
        let offset = offset.to_string();
        let limit = limit.to_string();
        let total = true.to_string();
        params.insert("total", total.as_str());
        params.insert("offset", offset.as_str());
        params.insert("limit", limit.as_str());
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_list(result, Parse::LikeAlbum)
    }

    /// 歌单详情
    /// songlist_id: 歌单 id
    pub async fn song_list_detail(&self, songlist_id: u64) -> Result<PlayListDetail> {
        let csrf_token = self.csrf.lock().await.borrow().to_owned();
        let path = "/weapi/v6/playlist/detail";
        let mut params = HashMap::new();
        let songlist_id = songlist_id.to_string();
        params.insert("id", songlist_id.as_str());
        params.insert("offset", "0");
        params.insert("total", "true");
        params.insert("limit", "1000");
        params.insert("n", "1000");
        params.insert("csrf_token", &csrf_token);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_mix_detail(&serde_json::from_str(&result)?)
    }

    /// 歌曲详情
    /// ids: 歌曲 id 列表
    pub async fn songs_detail(&self, ids: &[u64]) -> Result<Vec<SongInfo>> {
        let path = "/weapi/v3/song/detail";
        let mut params = HashMap::new();
        let c = ids
            .iter()
            .map(|i| format!("{{\\\"id\\\":\\\"{}\\\"}}", i))
            .collect::<Vec<String>>()
            .join(",");
        let c = format!("[{}]", c);
        params.insert("c", &c[..]);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_info(result, Parse::Usl)
    }

    /// 从网络下载图片
    /// url: 网址
    /// path: 本地保存路径(包含文件名)
    /// width: 宽度
    /// high: 高度
    #[allow(unused)]
    pub async fn download_img<I>(&self, url: I, path: PathBuf, width: u16, high: u16) -> Result<()>
    where
        I: Into<String>,
    {
        if !path.exists() {
            let url = url.into();
            let image_url = format!("{}?param={}y{}", url, width, high);

            let mut response = self.client.get_async(image_url).await?;
            if response.status().is_success() {
                let mut buf = vec![];
                response.copy_to(&mut buf).await?;
                fs::write(&path, buf)?;
            }
        }
        Ok(())
    }

    /// 从网络下载音乐
    /// url: 网址
    /// path: 本地保存路径(包含文件名)
    #[allow(unused)]
    pub async fn download_song<I>(&self, url: I, path: PathBuf) -> Result<()>
    where
        I: Into<String>,
    {
        if !path.exists() {
            let mut response = self.client.get_async(url.into()).await?;
            if response.status().is_success() {
                let mut buf = vec![];
                response.copy_to(&mut buf).await?;
                fs::write(&path, buf)?;
            }
        }
        Ok(())
    }
}

/// 底层网络接口封装
impl NcmApi {
    /// 设置使用代理
    /// proxy: 代理地址，支持以下协议
    ///   - http: Proxy. Default when no scheme is specified.
    ///   - https: HTTPS Proxy. (Added in 7.52.0 for OpenSSL, GnuTLS and NSS)
    ///   - socks4: SOCKS4 Proxy.
    ///   - socks4a: SOCKS4a Proxy. Proxy resolves URL hostname.
    ///   - socks5: SOCKS5 Proxy.
    ///   - socks5h: SOCKS5 Proxy. Proxy resolves URL hostname.
    pub fn set_proxy(&mut self, proxy: &str) -> Result<()> {
        if let Some(cookie_jar) = self.client.cookie_jar() {
            let client = HttpClient::builder()
                .timeout(Duration::from_secs(TIMEOUT))
                .proxy(Some(proxy.parse()?))
                .cookies()
                .cookie_jar(cookie_jar.to_owned())
                .build()
                .expect("初始化网络请求失败!");
            self.client = client;
        } else {
            let client = HttpClient::builder()
                .timeout(Duration::from_secs(TIMEOUT))
                .proxy(Some(proxy.parse()?))
                .cookies()
                .build()
                .expect("初始化网络请求失败!");
            self.client = client;
        }
        Ok(())
    }

    /// 发送请求
    /// method: 请求方法
    /// path: 请求路径
    /// params: 请求参数
    /// cryptoapi: 请求加密方式
    /// ua: 要使用的 USER_AGENT_LIST
    /// append_csrf: 是否在路径中添加 csrf
    async fn request(
        &self,
        method: Method,
        path: &str,
        params: HashMap<&str, &str>,
        cryptoapi: CryptoApi,
        ua: &str,
        append_csrf: bool,
    ) -> Result<String> {
        let mut csrf = self.csrf.lock().await.borrow().to_owned();
        if csrf.is_empty() {
            if let Some(cookies) = self.cookie_jar() {
                let uri = BASE_URL.parse()?;
                if let Some(cookie) = cookies.get_by_name(&uri, "__csrf") {
                    let __csrf = cookie.value().to_string();
                    self.csrf.lock().await.replace(__csrf.to_owned());
                    csrf = __csrf;
                }
            }
        }
        let mut url = format!("{}{}?csrf_token={}", BASE_URL, path, csrf);
        if !append_csrf {
            url = format!("{}{}", BASE_URL, path);
        }
        match method {
            Method::Post => {
                let user_agent = match cryptoapi {
                    CryptoApi::LinuxApi => LINUX_USER_AGNET.to_string(),
                    CryptoApi::Weapi => choose_user_agent(ua).to_string(),
                    CryptoApi::Eapi => choose_user_agent(ua).to_string(),
                };
                let body = match cryptoapi {
                    CryptoApi::LinuxApi => {
                        let data = format!(
                            r#"{{"method":"linuxapi","url":"{}","params":{}}}"#,
                            url.replace("weapi", "api"),
                            QueryParams::from_map(params).json()
                        );
                        Crypto::linuxapi(&data)
                    }
                    CryptoApi::Weapi => {
                        let mut params = params;
                        params.insert("csrf_token", &csrf);
                        Crypto::weapi(&QueryParams::from_map(params).json())
                    }
                    CryptoApi::Eapi => {
                        let mut params = params;
                        params.insert("csrf_token", &csrf);
                        url = path.to_string();
                        Crypto::eapi(
                            "/api/song/enhance/player/url",
                            &QueryParams::from_map(params).json(),
                        )
                    }
                };

                let request = Request::post(&url)
                    .header("Cookie", "os=pc; appver=2.7.1.198277")
                    .header("Accept", "*/*")
                    .header("Accept-Language", "en-US,en;q=0.5")
                    .header("Connection", "keep-alive")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .header("Host", "music.163.com")
                    .header("Referer", "https://music.163.com")
                    .header("User-Agent", user_agent)
                    .body(body)?;
                let mut response = self
                    .client
                    .send_async(request)
                    .await
                    .map_err(|_| anyhow!("none"))?;
                response.text().await.map_err(|_| anyhow!("none"))
            }
            Method::Get => self
                .client
                .get_async(&url)
                .await
                .map_err(|_| anyhow!("none"))?
                .text()
                .await
                .map_err(|_| anyhow!("none")),
        }
    }
}

fn choose_user_agent(ua: &str) -> &str {
    let index = if ua == "mobile" {
        rand::random::<usize>() % 7
    } else if ua == "pc" {
        rand::random::<usize>() % 5 + 8
    } else if !ua.is_empty() {
        return ua;
    } else {
        rand::random::<usize>() % USER_AGENT_LIST.len()
    };
    USER_AGENT_LIST[index]
}

#[cfg(test)]
mod tests {

    // use super::*;

    // #[async_std::test]
    // async fn test() {
    //     let api = NcmApi::new();
    //     assert!(api.banners().await.is_ok());
    // }
}
