use crate::config::ScreenEnum;
use anyhow::{anyhow, Result};
use ncm_play::PlayMode;
use crate::config::Command::SwitchPlayMode;

pub enum Command {
    Quit,
    GotoScreen(ScreenEnum),
    EnterCommand,
    Logout,
    PlayOrPause,
    SetVolume(f64),
    SwitchPlayMode(PlayMode),
    StartPlay,
    //
    Down,
    Up,
    NextPanel,
    PrevPanel,
    Esc,
    Play,
    //
    PrevTrack,
    NextTrack,
    Nop,
}

impl Command {
    pub fn parse(cmd_str: &str) -> Result<Self> {
        let mut tokens = cmd_str.split_whitespace();

        match tokens.next() {
            Some("q" | "quit" | "exit") => Ok(Self::Quit),
            Some("screen") => match tokens.next() {
                Some("1" | "main") => Ok(Self::GotoScreen(ScreenEnum::Main)),
                // Some("2" | "playlist" | "playlists") => Ok(Self::GotoScreen(ScreenEnum::Playlists)),
                Some("0" | "help") => Ok(Self::GotoScreen(ScreenEnum::Help)),
                Some(other) => Err(anyhow!("screen: Invalid screen identifier: {}", other)),
                None => Err(anyhow!("screen: Missing argument SCREEN_ID")),
            },
            Some("h" | "help") => Ok(Self::GotoScreen(ScreenEnum::Help)),
            Some("l" | "login") => Ok(Self::GotoScreen(ScreenEnum::Login)),
            Some("logout") => Ok(Self::Logout),
            Some("vol" | "volume") => match tokens.next() {
                Some(num) => {
                    if let Ok(vol) = num.parse::<f64>() {
                        Ok(Self::SetVolume(vol / 100.0))
                    } else {
                        Err(anyhow!("volume: Invalid argument NUMBER"))
                    }
                }
                None => Err(anyhow!("volume: Missing argument NUMBER")),
            },
            Some("mode") => match tokens.next() {
                Some("single") => Ok(Self::SwitchPlayMode(PlayMode::Single)),
                Some("sr" | "single-repeat") => Ok(SwitchPlayMode(PlayMode::SingleRepeat)),
                Some("lr" | "list-repeat") => Ok(SwitchPlayMode(PlayMode::ListRepeat)),
                Some("s" | "shuf" | "shuffle") => Ok(SwitchPlayMode(PlayMode::Shuffle)),
                Some(other) => Err(anyhow!("switch: Invalid play mode identifier: {}", other)),
                None => Err(anyhow!("switch: Missing argument PLAY_MODE")),
            }
            Some("start") => Ok(Self::StartPlay),
            Some(other) => Err(anyhow!("Invalid command: {}", other)),
            None => Ok(Self::Nop),
        }
    }
}
