use std::fs;
use std::path::PathBuf;

const APP_NAME: &str = "ncm-tui";

pub struct Path {
    // 一级目录
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,

    // 二级目录
    pub login_cookie: PathBuf,
    pub lyrics: PathBuf,
}

impl Path {
    pub fn new() -> Self {
        // 获取并验证各个目录路径
        let data = dirs_next::data_dir().unwrap_or_else(|| {
            panic!("Could not retrieve the data directory.");
        }).join(APP_NAME);
        if !data.exists() {
            fs::create_dir(&data).unwrap_or_else(|e| {
                panic!("Couldn't create data dir at {:?}: {}", data, e);
            });
        }

        let config = dirs_next::config_dir().unwrap_or_else(|| {
            panic!("Could not retrieve the config directory.");
        }).join(APP_NAME);
        if !config.exists() {
            fs::create_dir(&config).unwrap_or_else(|e| {
                panic!("Couldn't create config dir at {:?}: {}", config, e);
            });
        }

        let cache = dirs_next::cache_dir().unwrap_or_else(|| {
            panic!("Could not retrieve the cache directory.");
        }).join(APP_NAME);
        if !cache.exists() {
            fs::create_dir(&cache).unwrap_or_else(|e| {
                panic!("Couldn't create cache dir at {:?}: {}", cache, e);
            });
        }

        let login_cookie = data.clone().join("cookies.json");

        let lyrics = data.clone().join("lyrics");
        if !lyrics.exists() {
            fs::create_dir(&lyrics).unwrap_or_else(|e| {
                panic!("Couldn't create lyrics dir at {:?}: {}", lyrics, e);
            });
        }

        Self {
            data,
            config,
            cache,
            login_cookie,
            lyrics,
        }
    }
}
