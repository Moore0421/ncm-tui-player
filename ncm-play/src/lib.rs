use anyhow::Result;
use gstreamer_play::{gst, Play, PlayVideoRenderer};
use ncm_api::{NcmApi, SongInfo};
use tokio::sync::MutexGuard;

#[derive(Clone, PartialEq)]
pub enum PlayState {
    /// 未进入播放
    Stopped,

    /// 暂停
    Paused,

    /// 播放中
    Playing,

    /// 一首歌播放结束
    Ended,
}

pub enum PlayMode {
    Single,
    SingleRepeat,
    ListRepeat,
    Shuffle,
}

pub struct Player {
    play: Play,
    //
    play_state: PlayState,
    play_mode: PlayMode,
    //
    current_playlist_name: String,
    current_playlist: Vec<SongInfo>,
    //
    current_song_index: Option<usize>,
    current_song_info: Option<SongInfo>,
    current_song_lyrics: Option<Vec<(String, Option<String>)>>, // 兼容带翻译的歌词
    current_song_lyric_timestamps: Option<Vec<u64>>,            // 单位: ms
    current_song_lyric_index: Option<usize>,
}

impl Player {
    pub fn new() -> Self {
        gst::init().expect("Failed to initialize GST");

        let play = Play::new(None::<PlayVideoRenderer>);
        let mut config = play.config();
        config.set_user_agent(
            "User-Agent: Mozilla/5.0 (X11; Linux x86_64; rv:100.0) Gecko/20100101 Firefox/100.0",
        );
        config.set_position_update_interval(250);
        config.set_seek_accurate(true);
        play.set_config(config).unwrap();
        play.set_volume(0.2);

        Self {
            play,
            play_state: PlayState::Stopped,
            play_mode: PlayMode::ListRepeat,
            current_playlist_name: String::new(),
            current_playlist: Vec::new(),
            current_song_index: None,
            current_song_info: None,
            current_song_lyrics: None,
            current_song_lyric_timestamps: None,
            current_song_lyric_index: None,
        }
    }
}

/// setter & getter
impl Player {
    pub fn set_volume(&mut self, volume: f64) {
        self.play.set_volume(volume);
    }

    pub fn mute(&mut self) {
        self.play.set_volume(0.0);
    }

    pub fn volume(&self) -> f64 {
        self.play.volume()
    }

    pub fn is_playing(&self) -> bool {
        self.play_state == PlayState::Playing
    }

    pub fn play_state(&self) -> String {
        match self.play_state {
            PlayState::Stopped => String::from("pick a song to play :)"),
            PlayState::Paused => String::from("Paused"),
            PlayState::Playing => String::from("Playing"),
            PlayState::Ended => String::from("Single track ended"),
        }
    }

    pub fn duration(&self) -> Option<gst::ClockTime> {
        self.play.duration()
    }

    pub fn position(&self) -> Option<gst::ClockTime> {
        self.play.position()
    }

    pub fn set_play_mode(&mut self, play_mode: PlayMode) {
        self.play_mode = play_mode;
    }

    pub fn current_playlist_name_ref(&self) -> &String {
        &self.current_playlist_name
    }

    pub fn current_playlist(&self) -> Vec<SongInfo> {
        self.current_playlist.clone()
    }

    pub fn current_song_info_ref(&self) -> &Option<SongInfo> {
        &self.current_song_info
    }

    pub fn current_song_lyrics(&self) -> Option<Vec<(String, Option<String>)>> {
        self.current_song_lyrics.clone()
    }

    pub fn current_song_lyric_index(&self) -> Option<usize> {
        self.current_song_lyric_index
    }
}

/// public
impl Player {
    pub fn check_play_state(&mut self) {
        // if self.play_state == PlayState::Playing {
        //     if self.duration() == self.position() {
        //         self.play_state = PlayState::Stopped;
        //     }
        // }
    }

    pub fn play_or_pause(&mut self) {
        if self.play_state == PlayState::Playing {
            self.play.pause();
            self.play_state = PlayState::Paused;
        } else if self.play_state == PlayState::Paused {
            self.play.play();
            self.play_state = PlayState::Playing;
        }
    }

    pub fn switch_playlist(&mut self, playlist_name: String, playlist: Vec<SongInfo>) {
        self.current_playlist_name = playlist_name;
        self.current_playlist = playlist;
        self.current_song_index = if self.current_playlist.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    pub async fn play_particularly_now<'a>(
        &mut self,
        index_to_play: usize,
        ncm_api_guard: MutexGuard<'a, NcmApi>,
    ) -> Result<()> {
        if index_to_play < self.current_playlist.len() {
            self.play_state = PlayState::Playing;
            self.current_song_index = Some(index_to_play);
            self.current_song_info = Some(self.current_playlist[index_to_play].clone());

            self.play_next(ncm_api_guard).await?;
        }

        Ok(())
    }

    pub async fn auto_play<'a>(&mut self, ncm_api_guard: MutexGuard<'a, NcmApi>) -> Result<()> {
        // 判断一首歌是否播放完
        if self.play_state == PlayState::Playing {
            if self.duration() == self.position() {
                self.play_state = PlayState::Ended;
            }
        }

        if self.play_state == PlayState::Playing {
            // 当前歌曲仍在播放，推进歌词
            self.auto_lyric_forward();
        } else if self.play_state == PlayState::Ended {
            // 播放下一首
            self.update_next_to_play();
            self.play_next(ncm_api_guard).await?;
        }

        Ok(())
    }
}

/// private
impl Player {
    fn update_next_to_play(&mut self) {
        self.current_song_info = match self.play_mode {
            PlayMode::Single => None,
            PlayMode::SingleRepeat => self.current_song_info.clone(),
            PlayMode::ListRepeat => {
                if let Some(mut index) = self.current_song_index {
                    index += 1;
                    if index >= self.current_playlist.len() {
                        index = 0;
                    }
                    self.current_song_index = Some(index);
                    Some(self.current_playlist[index].clone())
                } else {
                    None
                }
            }
            PlayMode::Shuffle => None,
        };
    }

    fn play_new_song_by_uri(&mut self, uri: &str) {
        self.play.stop();
        self.play.set_uri(Some(uri));
        self.play.play();
        self.play_state = PlayState::Playing;
    }

    async fn play_next<'a>(&mut self, ncm_api_guard: MutexGuard<'a, NcmApi>) -> Result<()> {
        if let Some(mut song_info) = self.current_song_info.clone() {
            // 获取歌曲 uri
            song_info.song_url = ncm_api_guard.get_song_url(song_info.id).await?.url;
            self.current_song_info = Some(song_info.clone());

            // 获取歌词
            self.update_current_lyric_encoded(ncm_api_guard).await?;

            // 播放
            self.play_new_song_by_uri(song_info.song_url.as_str());

            // 播放状态
            self.play_state = PlayState::Playing;
        } else {
            // 播放状态
            self.play_state = PlayState::Stopped;
        }

        Ok(())
    }

    async fn update_current_lyric_encoded<'a>(
        &mut self,
        ncm_api_guard: MutexGuard<'a, NcmApi>,
    ) -> Result<()> {
        if let Some(current_song_info) = self.current_song_info.clone() {
            if let Ok(lyric_with_timestamp) = ncm_api_guard.song_lyric(current_song_info).await {
                // 获取歌词和时间戳（在 ncm-api 中已编码过）
                let mut lyrics: Vec<(String, Option<String>)> = Vec::new();
                let mut timestamps: Vec<u64> = Vec::new();
                for (timestamp, lyric) in lyric_with_timestamp {
                    lyrics.push(lyric);
                    timestamps.push(timestamp);
                }

                self.current_song_lyrics = Some(lyrics);
                self.current_song_lyric_timestamps = Some(timestamps);
                self.current_song_lyric_index = Some(0);
            }
        }

        Ok(())
    }

    fn auto_lyric_forward(&mut self) {
        if let (Some(current_song_lyric_index), Some(current_song_lyric_timestamps)) = (
            self.current_song_lyric_index,
            self.current_song_lyric_timestamps.clone(),
        ) {
            if let Some(current_position) = self.position() {
                if current_song_lyric_index < current_song_lyric_timestamps.len() - 1 {
                    let next_timestamp =
                        current_song_lyric_timestamps[current_song_lyric_index + 1];

                    // 已经到下一句歌词的时间戳
                    if current_position.mseconds() >= next_timestamp {
                        self.current_song_lyric_index = Some(current_song_lyric_index + 1);
                    }
                }
            }
        }
    }
}
