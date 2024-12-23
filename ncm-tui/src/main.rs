mod config;
mod ui;

use crate::config::Path;
use crate::ui::App;
use anyhow::Result;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use crossterm::{event, execute};
use lazy_static::lazy_static;
use ncm_api::NcmApi;
use ncm_play::Player;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task;
use tokio::time::sleep;

const POLL_DURATION: Duration = Duration::from_millis(100);

lazy_static! {
    static ref PATH_CONFIG: Path = Path::new();
    static ref NCM_API: Arc<Mutex<NcmApi>> = Arc::new(Mutex::new(NcmApi::from_cookie_jar(
        PATH_CONFIG.login_cookie.clone(),
        PATH_CONFIG.lyrics.clone(),
        PATH_CONFIG.cache.clone(),
    )));
    static ref PLAYER: Arc<Mutex<Player>> = Arc::new(Mutex::new(Player::new()));
}

#[tokio::main]
async fn main() -> Result<()> {
    // env_logger::init();

    let app = Arc::new(Mutex::new(App::new(create_terminal()?)));

    // 绘制第一帧（launch screen）
    app.lock().await.draw_launch_screen()?;

    // 创建 NCM_API 时会默认尝试 cookie 登录，在新线程中检查 cookie 状态并初始化
    let app2 = Arc::clone(&app);
    let ncm_api_2 = Arc::clone(&NCM_API);
    task::spawn(async move {
        sleep(Duration::from_secs(1)).await; // 给启动帧留缓冲

        if ncm_api_2
            .lock()
            .await
            .check_cookie_login()
            .await
            .unwrap_or(false)
        {
            app2.lock()
                .await
                .init_after_login()
                .await
                .expect("Couldn't initialize application");
        } else {
            app2.lock().await.init_after_no_login().await;
        }
    });

    loop {
        // 检查播放情况
        PLAYER.lock().await.auto_play(NCM_API.lock().await).await?;

        // 根据 Controller 流程，先执行 update_model()，再执行 handle_event()
        app.lock().await.update_model().await?;

        if event::poll(POLL_DURATION)? {
            if !app.lock().await.handle_event().await? {
                return app.lock().await.restore_terminal();
            }
        }

        // 渲染
        app.lock().await.draw()?;
    }
}

fn create_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    Ok(terminal)
}
