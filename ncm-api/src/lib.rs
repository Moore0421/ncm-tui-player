//
// mod.rs
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
use log::error;
use regex::Regex;
use std::{collections::HashMap, fs, io, path::PathBuf, time::Duration};
use std::sync::{Arc};
use tokio::sync::Mutex;
use urlqstring::QueryParams;

lazy_static! {
    static ref _CSRF: Regex = Regex::new(r"_csrf=(?P<csrf>[^(;|$)]+)").unwrap();
}

#[derive(Clone)]
pub struct NcmApi {
    client: HttpClient,
    // csrf: RefCell<String>,
    csrf: Arc<Mutex<String>>,
    cookie_path: PathBuf,
    is_login: bool,
    login_info: Option<LoginInfo>,

    pub user_favorite_songlist_name: String,
    pub user_favorite_songlist: Option<Vec<SongInfo>>,
}

#[allow(unused)]
enum CryptoApi {
    Weapi,
    LinuxApi,
    Eapi,
}

impl NcmApi {
    pub fn new(data_path: &PathBuf) -> Self {
        let client = HttpClient::builder()
            .timeout(Duration::from_secs(TIMEOUT))
            .max_connections(DEFAULT_MAX_CONNECTIONS)
            .cookies()
            .build()
            .expect("初始化网络请求失败!");
        Self {
            client,
            csrf: Arc::new(Mutex::new(String::new())),
            cookie_path: data_path.clone().join(COOKIE_FILE),
            is_login: false,
            login_info: None,
            user_favorite_songlist_name: String::from(""),
            user_favorite_songlist: None,
        }
    }

    pub fn from_cookie_jar(data_path: &PathBuf) -> Self {
        if let Some(cookie_jar) =
            Self::load_cookie_jar_from_file(data_path.clone().join(COOKIE_FILE))
        {
            Self {
                client: Self::create_client_from_cookie_jar(cookie_jar),
                csrf: Arc::new(Mutex::new(String::new())),
                cookie_path: data_path.clone().join(COOKIE_FILE),
                is_login: false,
                login_info: None,
                user_favorite_songlist_name: String::from(""),
                user_favorite_songlist: None,
            }
        } else {
            Self::new(data_path)
        }
    }
}

/// Cookie
impl NcmApi {
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
                let (user_favorite_songlist_name, user_favorite_songlist) = self.user_favorite_songlist().await?;
                self.user_favorite_songlist_name = user_favorite_songlist_name;
                self.user_favorite_songlist = Some(user_favorite_songlist);

                Ok(true)
            },
            Err(_err) => Ok(false),
        }
    }

    /// 新账号已验证登录后，初始化
    pub async fn init_after_new_login(&mut self) -> Result<()> {
        self.store_cookie();

        self.login_info = Some(self.login_status().await?);
        self.is_login = true;
        let (user_favorite_songlist_name, user_favorite_songlist) = self.user_favorite_songlist().await?;
        self.user_favorite_songlist_name = user_favorite_songlist_name;
        self.user_favorite_songlist = Some(user_favorite_songlist);

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

    pub fn is_login(&self) -> bool {
        self.is_login
    }

    pub fn login_info(&self) -> Option<LoginInfo> {
        self.login_info.clone()
    }
}

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
        let mut csrf = self.csrf.lock().await;
        // let mut csrf = self.csrf.borrow().to_owned();
        if csrf.is_empty() {
            if let Some(cookies) = self.cookie_jar() {
                let uri = BASE_URL.parse()?;
                if let Some(cookie) = cookies.get_by_name(&uri, "__csrf") {
                    let __csrf = cookie.value().to_string();
                    // self.csrf.replace(__csrf.to_owned());
                    *csrf = __csrf;
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

impl NcmApi {
    /// 每日签到
    #[allow(unused)]
    pub async fn daily_task(&self) -> Result<Msg> {
        let path = "/weapi/point/dailyTask";
        let mut params = HashMap::new();
        params.insert("type", "0");
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_msg(result)
    }

    pub async fn user_favorite_songlist(&self) -> Result<(String, Vec<SongInfo>)> {
        match &self.login_info {
            Some(login_info) => {
                let user_id = login_info.uid.clone();

                match self.user_song_list(user_id, 0, 1).await {
                    Ok(user_songlists) => {
                        if !user_songlists.is_empty() {
                            Ok((
                                user_songlists[0].name.clone(),
                                self.song_list_detail(user_songlists[0].id).await?.songs
                            ))
                        } else {
                            Err(anyhow!("user has no songlist."))
                        }
                    },
                    Err(err) => Err(err),
                }
            },
            None => Err(anyhow!("you have to login first.")),
        }
    }

    /// 用户音乐id列表
    /// uid: 用户id
    #[allow(unused)]
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

    /// 用户歌单
    /// uid: 用户id
    /// offset: 列表起点号
    /// limit: 列表长度
    #[allow(unused)]
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

    /// 用户收藏专辑列表
    /// offset: 列表起点号
    /// limit: 列表长度
    #[allow(unused)]
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

    /// 用户云盘
    #[allow(unused)]
    pub async fn user_cloud_disk(&self) -> Result<Vec<SongInfo>> {
        let path = "/weapi/v1/cloud/get";
        let mut params = HashMap::new();
        params.insert("offset", "0");
        params.insert("limit", "10000");
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_info(result, Parse::Ucd)
    }

    /// 歌单详情
    /// songlist_id: 歌单 id
    #[allow(unused)]
    pub async fn song_list_detail(&self, songlist_id: u64) -> Result<PlayListDetail> {
        // let csrf_token = self.csrf.borrow().to_owned();
        let csrf_token = self.csrf.lock().await
            .clone();
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
    #[allow(unused)]
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

    /// 歌曲 URL
    /// ids: 歌曲列表
    /// br: 歌曲码率
    ///     l: 128000
    ///     m: 192000
    ///     h: 320000
    ///    sq: 999000
    ///    hr: 1900000
    #[allow(unused)]
    pub async fn songs_url(&self, ids: &[u64], br: &str) -> Result<Vec<SongUrl>> {
        // 使用 WEBAPI 获取音乐
        // let csrf_token = self.csrf.borrow().to_owned();
        // let path = "/weapi/song/enhance/player/url/v1";
        // let mut params = HashMap::new();
        // let ids = serde_json::to_string(ids)?;
        // params.insert("ids", ids.as_str());
        // params.insert("level", "standard");
        // params.insert("encodeType", "aac");
        // params.insert("csrf_token", &csrf_token);
        // let result = self
        //     .request(Method::Post, path, params, CryptoApi::Weapi, "")
        //     .await?;

        // 使用 Eapi 获取音乐
        let path = "https://interface3.music.163.com/eapi/song/enhance/player/url";
        let mut params = HashMap::new();
        let ids = serde_json::to_string(ids)?;
        params.insert("ids", ids.as_str());
        params.insert("br", br);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Eapi, "", true)
            .await?;
        to_song_url(result)
    }

    /// 每日推荐歌单
    #[allow(unused)]
    pub async fn recommend_resource(&self) -> Result<Vec<SongList>> {
        let path = "/weapi/v1/discovery/recommend/resource";
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
        to_song_list(result, Parse::Rmd)
    }

    /// 每日推荐歌曲
    #[allow(unused)]
    pub async fn recommend_songs(&self) -> Result<Vec<SongInfo>> {
        let path = "/weapi/v2/discovery/recommend/songs";
        let mut params = HashMap::new();
        params.insert("total", "ture");
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_info(result, Parse::Rmds)
    }

    /// 私人FM
    #[allow(unused)]
    pub async fn personal_fm(&self) -> Result<Vec<SongInfo>> {
        let path = "/weapi/v1/radio/get";
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
        to_song_info(result, Parse::Rmd)
    }

    /// 收藏/取消收藏
    /// songid: 歌曲id
    /// like: true 收藏，false 取消
    #[allow(unused)]
    pub async fn like(&self, like: bool, songid: u64) -> bool {
        let path = "/weapi/radio/like";
        let mut params = HashMap::new();
        let songid = songid.to_string();
        let like = like.to_string();
        params.insert("alg", "itembased");
        params.insert("trackId", songid.as_str());
        params.insert("like", like.as_str());
        params.insert("time", "25");
        if let Ok(result) = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await
        {
            return to_msg(result)
                .unwrap_or(Msg {
                    code: 0,
                    msg: "".to_owned(),
                })
                .code
                .eq(&200);
        }
        false
    }

    /// FM 不喜欢
    /// songid: 歌曲id
    #[allow(unused)]
    pub async fn fm_trash(&self, songid: u64) -> bool {
        let path = "/weapi/radio/trash/add";
        let mut params = HashMap::new();
        let songid = songid.to_string();
        params.insert("alg", "RT");
        params.insert("songId", songid.as_str());
        params.insert("time", "25");
        if let Ok(result) = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await
        {
            return to_msg(result)
                .unwrap_or(Msg {
                    code: 0,
                    msg: "".to_owned(),
                })
                .code
                .eq(&200);
        }
        false
    }

    /// 搜索
    /// keywords: 关键词
    /// types: 1: 单曲, 10: 专辑, 100: 歌手, 1000: 歌单, 1002: 用户, 1004: MV, 1006: 歌词, 1009: 电台, 1014: 视频
    /// offset: 起始点
    /// limit: 数量
    #[allow(unused)]
    pub async fn search(
        &self,
        keywords: String,
        types: u32,
        offset: u16,
        limit: u16,
    ) -> Result<String> {
        let path = "/weapi/search/get";
        let mut params = HashMap::new();
        let _types = types.to_string();
        let offset = offset.to_string();
        let limit = limit.to_string();
        params.insert("s", &keywords[..]);
        params.insert("type", &_types[..]);
        params.insert("offset", &offset[..]);
        params.insert("limit", &limit[..]);
        self.request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await
    }

    /// 搜索单曲
    /// keywords: 关键词
    /// offset: 起始点
    /// limit: 数量
    #[allow(unused)]
    pub async fn search_song(
        &self,
        keywords: String,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<SongInfo>> {
        let result = self.search(keywords, 1, offset, limit).await?;
        to_song_info(result, Parse::Search)
    }

    /// 搜索歌手
    /// keywords: 关键词
    /// offset: 起始点
    /// limit: 数量
    #[allow(unused)]
    pub async fn search_singer(
        &self,
        keywords: String,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<SingerInfo>> {
        let result = self.search(keywords, 100, offset, limit).await?;
        to_singer_info(result)
    }

    /// 搜索专辑
    /// keywords: 关键词
    /// offset: 起始点
    /// limit: 数量
    #[allow(unused)]
    pub async fn search_album(
        &self,
        keywords: String,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<SongList>> {
        let result = self.search(keywords, 10, offset, limit).await?;
        to_song_list(result, Parse::SearchAlbum)
    }

    /// 搜索歌单
    /// keywords: 关键词
    /// offset: 起始点
    /// limit: 数量
    #[allow(unused)]
    pub async fn search_songlist(
        &self,
        keywords: String,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<SongList>> {
        let result = self.search(keywords, 1000, offset, limit).await?;
        to_song_list(result, Parse::Search)
    }

    /// 搜索歌词
    /// keywords: 关键词
    /// offset: 起始点
    /// limit: 数量
    #[allow(unused)]
    pub async fn search_lyrics(
        &self,
        keywords: String,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<SongInfo>> {
        let result = self.search(keywords, 1006, offset, limit).await?;
        to_song_info(result, Parse::Search)
    }

    /// 获取歌手热门单曲
    /// id: 歌手 ID
    #[allow(unused)]
    pub async fn singer_songs(&self, id: u64) -> Result<Vec<SongInfo>> {
        let path = format!("/weapi/v1/artist/{}", id);
        let mut params = HashMap::new();
        let result = self
            .request(Method::Post, &path, params, CryptoApi::Weapi, "", false)
            .await?;
        to_song_info(result, Parse::Singer)
    }

    /// 获取歌手全部单曲
    /// id: 歌手 ID
    /// order: 排序方式:
    //	      "hot": 热门
    ///       "time": 时间
    /// offset: 起始点
    /// limit: 数量
    #[allow(unused)]
    pub async fn singer_all_songs(
        &self,
        id: u64,
        order: &str,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<SongInfo>> {
        let path = "/weapi/v1/artist/songs";
        let mut params = HashMap::new();
        let id = id.to_string();
        let offset = offset.to_string();
        let limit = limit.to_string();
        params.insert("id", &id[..]);
        params.insert("private_cloud", "true");
        params.insert("work_type", "1");
        params.insert("order", order);
        params.insert("offset", &offset[..]);
        params.insert("limit", &limit[..]);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", false)
            .await?;
        to_song_info(result, Parse::SingerSongs)
    }

    /// 全部新碟
    /// offset: 起始点
    /// limit: 数量
    /// area: ALL:全部,ZH:华语,EA:欧美,KR:韩国,JP:日本
    #[allow(unused)]
    pub async fn new_albums(&self, area: &str, offset: u16, limit: u16) -> Result<Vec<SongList>> {
        let path = "/weapi/album/new";
        let mut params = HashMap::new();
        let offset = offset.to_string();
        let limit = limit.to_string();
        params.insert("area", area);
        params.insert("offset", &offset[..]);
        params.insert("limit", &limit[..]);
        params.insert("total", "true");
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_list(result, Parse::Album)
    }

    /// 专辑
    /// album_id: 专辑 id
    #[allow(unused)]
    pub async fn album(&self, album_id: u64) -> Result<AlbumDetail> {
        let path = format!("/weapi/v1/album/{}", album_id);
        let result = self
            .request(
                Method::Post,
                &path,
                HashMap::new(),
                CryptoApi::Weapi,
                "",
                true,
            )
            .await?;
        to_album_detail(&serde_json::from_str(&result)?)
    }

    /// 歌单动态信息
    /// songlist_id: 歌单 id
    #[allow(unused)]
    pub async fn songlist_detail_dynamic(&self, songlist_id: u64) -> Result<PlayListDetailDynamic> {
        let path = "/weapi/playlist/detail/dynamic";
        let mut params = HashMap::new();
        let id = songlist_id.to_string();
        params.insert("id", &id[..]);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_songlist_detail_dynamic(result)
    }

    /// 专辑动态信息
    /// album_id: 专辑 id
    #[allow(unused)]
    pub async fn album_detail_dynamic(&self, album_id: u64) -> Result<AlbumDetailDynamic> {
        let path = "/weapi/album/detail/dynamic";
        let mut params = HashMap::new();
        let id = album_id.to_string();
        params.insert("id", &id[..]);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_album_detail_dynamic(result)
    }

    /// 热门推荐歌单
    /// offset: 起始点
    /// limit: 数量
    /// order: 排序方式:
    //	      "hot": 热门，
    ///        "new": 最新
    /// cat: 全部,华语,欧美,日语,韩语,粤语,小语种,流行,摇滚,民谣,电子,舞曲,说唱,轻音乐,爵士,乡村,R&B/Soul,古典,民族,英伦,金属,朋克,蓝调,雷鬼,世界音乐,拉丁,另类/独立,New Age,古风,后摇,Bossa Nova,清晨,夜晚,学习,工作,午休,下午茶,地铁,驾车,运动,旅行,散步,酒吧,怀旧,清新,浪漫,性感,伤感,治愈,放松,孤独,感动,兴奋,快乐,安静,思念,影视原声,ACG,儿童,校园,游戏,70后,80后,90后,网络歌曲,KTV,经典,翻唱,吉他,钢琴,器乐,榜单,00后
    #[allow(unused)]
    pub async fn top_song_list(
        &self,
        cat: &str,
        order: &str,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<SongList>> {
        let path = "/weapi/playlist/list";
        let mut params = HashMap::new();
        let offset = offset.to_string();
        let limit = limit.to_string();
        params.insert("cat", cat);
        params.insert("order", order);
        params.insert("total", "true");
        params.insert("offset", &offset[..]);
        params.insert("limit", &limit[..]);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_list(result, Parse::Top)
    }

    /// 精品歌单
    /// lasttime: 分页参数,取上一页最后一个歌单的 updateTime 获取下一页数据
    /// limit: 数量
    /// cat: 全部,华语,欧美,韩语,日语,粤语,小语种,运动,ACG,影视原声,流行,摇滚,后摇,古风,民谣,轻音乐,电子,器乐,说唱,古典,爵士
    #[allow(unused)]
    pub async fn top_song_list_highquality(
        &self,
        cat: &str,
        lasttime: u8,
        limit: u8,
    ) -> Result<Vec<SongList>> {
        let path = "/api/playlist/highquality/list";
        let mut params = HashMap::new();
        let lasttime = lasttime.to_string();
        let limit = limit.to_string();
        params.insert("cat", cat);
        params.insert("total", "true");
        params.insert("lasttime", &lasttime[..]);
        params.insert("limit", &limit[..]);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_list(result, Parse::Top)
    }

    /// 获取排行榜
    #[allow(unused)]
    pub async fn toplist(&self) -> Result<Vec<TopList>> {
        let path = "/api/toplist";
        let mut params = HashMap::new();
        let res = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_toplist(res)
    }

    /// 热门歌曲/排行榜
    /// list_id:
    /// 云音乐飙升榜: 19723756
    /// 云音乐新歌榜: 3779629
    /// 网易原创歌曲榜: 2884035
    /// 云音乐热歌榜: 3778678
    /// 云音乐古典音乐榜: 71384707
    /// 云音乐ACG音乐榜: 71385702
    /// 云音乐韩语榜: 745956260
    /// 云音乐国电榜: 10520166
    /// 云音乐嘻哈榜: 991319590']
    /// 抖音排行榜: 2250011882
    /// UK排行榜周榜: 180106
    /// 美国Billboard周榜: 60198
    /// KTV嗨榜: 21845217
    /// iTunes榜: 11641012
    /// Hit FM Top榜: 120001
    /// 日本Oricon周榜: 60131
    /// 台湾Hito排行榜: 112463
    /// 香港电台中文歌曲龙虎榜: 10169002
    /// 华语金曲榜: 4395559
    #[allow(unused)]
    pub async fn top_songs(&self, list_id: u64) -> Result<PlayListDetail> {
        self.song_list_detail(list_id).await
    }

    /// 查询歌词
    /// music_id: 歌曲id
    #[allow(unused)]
    pub async fn song_lyric(&self, music_id: u64) -> Result<Lyrics> {
        // let csrf_token = self.csrf.borrow().to_owned();
        let csrf_token = self.csrf.lock().await;
        let path = "/weapi/song/lyric";
        let mut params = HashMap::new();
        let id = music_id.to_string();
        params.insert("id", &id[..]);
        params.insert("lv", "-1");
        params.insert("tv", "-1");
        params.insert("csrf_token", &csrf_token);
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_lyric(result)
    }

    /// 收藏/取消收藏歌单
    /// like: true 收藏，false 取消
    /// id: 歌单 id
    #[allow(unused)]
    pub async fn song_list_like(&self, like: bool, id: u64) -> bool {
        let path = if like {
            "/weapi/playlist/subscribe"
        } else {
            "/weapi/playlist/unsubscribe"
        };
        let mut params = HashMap::new();
        let id = id.to_string();
        params.insert("id", &id[..]);
        if let Ok(result) = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await
        {
            return to_msg(result)
                .unwrap_or(Msg {
                    code: 0,
                    msg: "".to_owned(),
                })
                .code
                .eq(&200);
        }
        false
    }

    /// 收藏/取消收藏专辑
    /// like: true 收藏，false 取消
    /// id: 歌单 id
    #[allow(unused)]
    pub async fn album_like(&self, like: bool, id: u64) -> bool {
        let path = if like {
            "/api/album/sub"
        } else {
            "/api/album/unsub"
        };
        let path = format!("{}?id={}", path, id);
        let mut params = HashMap::new();
        let id = id.to_string();
        params.insert("id", id.as_str());
        if let Ok(result) = self
            .request(Method::Post, &path, params, CryptoApi::Weapi, "", false)
            .await
        {
            return to_msg(result)
                .unwrap_or(Msg {
                    code: 0,
                    msg: "".to_owned(),
                })
                .code
                .eq(&200);
        }
        false
    }

    /// 获取 APP 首页信息
    #[allow(unused)]
    pub async fn homepage(&self, client_type: ClientType) -> Result<String> {
        let path = "/api/homepage/block/page";
        let mut params = HashMap::new();
        params.insert("refresh", "false");
        params.insert("cursor", "null");
        self.request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await
    }

    /// 获取首页轮播图
    #[allow(unused)]
    pub async fn banners(&self) -> Result<Vec<BannersInfo>> {
        let path = "/weapi/v2/banner/get";
        let mut params = HashMap::new();
        params.insert("clientType", "pc");
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_banners_info(result)
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

    /// 用户电台定阅列表
    /// offset: 列表起点号
    /// limit: 列表长度
    #[allow(unused)]
    pub async fn user_radio_sublist(&self, offset: u16, limit: u16) -> Result<Vec<SongList>> {
        let path = "/weapi/djradio/get/subed";
        let mut params = HashMap::new();
        let offset = offset.to_string();
        let limit = limit.to_string();
        params.insert("total", "true");
        params.insert("offset", offset.as_str());
        params.insert("limit", limit.as_str());
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_list(result, Parse::Radio)
    }

    /// 电台节目列表
    /// rid: 电台ID
    /// offset: 列表起点号
    /// limit: 列表长度
    #[allow(unused)]
    pub async fn radio_program(&self, rid: u64, offset: u16, limit: u16) -> Result<Vec<SongInfo>> {
        let path = "/weapi/dj/program/byradio";
        let mut params = HashMap::new();
        let id = rid.to_string();
        let offset = offset.to_string();
        let limit = limit.to_string();
        params.insert("radioId", id.as_str());
        params.insert("offset", offset.as_str());
        params.insert("limit", limit.as_str());
        params.insert("asc", "false");
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_info(result, Parse::Radio)
    }

    /// 心动模式/智能播放
    /// song_id: 歌曲ID
    /// playlist_id: 歌单ID
    #[allow(unused)]
    pub async fn playmode_intelligence_list(&self, sid: u64, pid: u64) -> Result<Vec<SongInfo>> {
        let path = "/weapi/playmode/intelligence/list";
        let mut params = HashMap::new();
        let id = sid.to_string();
        let pid = pid.to_string();
        params.insert("songId", id.as_str());
        params.insert("type", "fromPlayOne");
        params.insert("playlistId", pid.as_str());
        params.insert("startMusicId", id.as_str());
        params.insert("count", "1");
        let result = self
            .request(Method::Post, path, params, CryptoApi::Weapi, "", true)
            .await?;
        to_song_info(result, Parse::Intelligence)
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
