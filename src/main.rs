// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    app::{message, Command, Core, Settings},
    cosmic_config::{self, CosmicConfigEntry},
    cosmic_theme, executor,
    iced::{
        event::{self, Event},
        keyboard::{Event as KeyEvent, KeyCode, Modifiers},
        subscription::{self, Subscription},
        window, Alignment, Length,
    },
    widget, Application, ApplicationExt, Element,
};
use std::{
    any::TypeId,
    collections::HashMap,
    env,
    path::PathBuf,
    process,
    sync::{mpsc, Arc, Mutex},
    time::Instant,
};

use config::{AppTheme, Config, CONFIG_VERSION};
mod config;

use key_bind::{key_binds, KeyBind};
mod key_bind;

mod localize;

use player::{PlayerMessage, VideoFrame};
mod player;

/// Runs application with these settings
#[rustfmt::skip]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    localize::localize();

    let (config_handler, config) = match cosmic_config::Config::new(App::APP_ID, CONFIG_VERSION) {
        Ok(config_handler) => {
            let config = match Config::get_entry(&config_handler) {
                Ok(ok) => ok,
                Err((errs, config)) => {
                    log::info!("errors loading config: {:?}", errs);
                    config
                }
            };
            (Some(config_handler), config)
        }
        Err(err) => {
            log::error!("failed to create config handler: {}", err);
            (None, Config::default())
        }
    };

    //TODO: support multiple paths
    let path = match env::args().skip(1).next() {
        Some(arg) => PathBuf::from(arg),
        None => {
            log::error!("no argument provided");
            process::exit(1);
        }
    };

    let (player_tx, video_frame_lock) = player::run(path);

    let mut settings = Settings::default();
    settings = settings.theme(config.app_theme.theme());

    #[cfg(target_os = "redox")]
    {
        // Redox does not support resize if doing CSDs
        settings = settings.client_decorations(false);
    }

    //TODO: allow size limits on iced_winit
    //settings = settings.size_limits(Limits::NONE.min_width(400.0).min_height(200.0));

    let flags = Flags {
        config_handler,
        config,
        player_tx,
        video_frame_lock,
    };
    cosmic::app::run::<App>(settings, flags)?;

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Todo,
    SeekBackward,
    SeekForward,
}

impl Action {
    pub fn message(&self) -> Message {
        match self {
            Self::Todo => Message::Todo,
            Self::SeekBackward => Message::Player(PlayerMessage::SeekRelative(-10.0)),
            Self::SeekForward => Message::Player(PlayerMessage::SeekRelative(10.0)),
        }
    }
}

#[derive(Clone)]
pub struct Flags {
    config_handler: Option<cosmic_config::Config>,
    config: Config,
    player_tx: mpsc::Sender<PlayerMessage>,
    video_frame_lock: Arc<Mutex<Option<VideoFrame>>>,
}

/// Messages that are used specifically by our [`App`].
#[derive(Clone, Debug)]
pub enum Message {
    Todo,
    AppTheme(AppTheme),
    Config(Config),
    Key(Modifiers, KeyCode),
    Player(PlayerMessage),
    SystemThemeModeChange(cosmic_theme::ThemeMode),
    Tick(Instant),
    ToggleContextPage(ContextPage),
    WindowClose,
    WindowNew,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextPage {
    Settings,
}

impl ContextPage {
    fn title(&self) -> String {
        match self {
            Self::Settings => fl!("settings"),
        }
    }
}

/// The [`App`] stores application-specific state.
pub struct App {
    core: Core,
    flags: Flags,
    app_themes: Vec<String>,
    context_page: ContextPage,
    key_binds: HashMap<KeyBind, Action>,
    handle_opt: Option<widget::image::Handle>,
}

impl App {
    fn update_config(&mut self) -> Command<Message> {
        cosmic::app::command::set_theme(self.flags.config.app_theme.theme())
    }

    fn update_title(&mut self) -> Command<Message> {
        let title = "COSMIC Media Player";
        self.set_header_title(title.to_string());
        self.set_window_title(title.to_string())
    }

    fn settings(&self) -> Element<Message> {
        let app_theme_selected = match self.flags.config.app_theme {
            AppTheme::Dark => 1,
            AppTheme::Light => 2,
            AppTheme::System => 0,
        };
        widget::settings::view_column(vec![widget::settings::view_section(fl!("appearance"))
            .add(
                widget::settings::item::builder(fl!("theme")).control(widget::dropdown(
                    &self.app_themes,
                    Some(app_theme_selected),
                    move |index| {
                        Message::AppTheme(match index {
                            1 => AppTheme::Dark,
                            2 => AppTheme::Light,
                            _ => AppTheme::System,
                        })
                    },
                )),
            )
            .into()])
        .into()
    }
}

/// Implement [`Application`] to integrate with COSMIC.
impl Application for App {
    /// Default async executor to use with the app.
    type Executor = executor::Default;

    /// Argument received
    type Flags = Flags;

    /// Message type specific to our [`App`].
    type Message = Message;

    /// The unique application ID to supply to the window manager.
    const APP_ID: &'static str = "com.system76.CosmicPlayer";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    /// Creates the application, and optionally emits command on initialize.
    fn init(core: Core, flags: Self::Flags) -> (Self, Command<Self::Message>) {
        let app_themes = vec![fl!("match-desktop"), fl!("dark"), fl!("light")];
        let mut app = App {
            core,
            flags,
            app_themes,
            context_page: ContextPage::Settings,
            key_binds: key_binds(),
            handle_opt: None,
        };

        let command = app.update_title();
        (app, command)
    }

    fn on_escape(&mut self) -> Command<Message> {
        if self.core.window.show_context {
            // Close context drawer if open
            self.core.window.show_context = false;
        }
        Command::none()
    }

    /// Handle application events here.
    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        // Helper for updating config values efficiently
        macro_rules! config_set {
            ($name: ident, $value: expr) => {
                match &self.flags.config_handler {
                    Some(config_handler) => {
                        match paste::paste! { self.flags.config.[<set_ $name>](config_handler, $value) } {
                            Ok(_) => {}
                            Err(err) => {
                                log::warn!(
                                    "failed to save config {:?}: {}",
                                    stringify!($name),
                                    err
                                );
                            }
                        }
                    }
                    None => {
                        self.flags.config.$name = $value;
                        log::warn!(
                            "failed to save config {:?}: no config handler",
                            stringify!($name)
                        );
                    }
                }
            };
        }

        match message {
            Message::Todo => {
                log::warn!("TODO");
            }
            Message::AppTheme(app_theme) => {
                config_set!(app_theme, app_theme);
                return self.update_config();
            }
            Message::Config(config) => {
                if config != self.flags.config {
                    log::info!("update config");
                    //TODO: update syntax theme by clearing tabs, only if needed
                    self.flags.config = config;
                    return self.update_config();
                }
            }
            Message::Key(modifiers, key_code) => {
                for (key_bind, action) in self.key_binds.iter() {
                    if key_bind.matches(modifiers, key_code) {
                        return self.update(action.message());
                    }
                }
            }
            Message::Player(player_message) => {
                self.flags.player_tx.send(player_message).unwrap();
            }
            Message::SystemThemeModeChange(_theme_mode) => {
                return self.update_config();
            }
            Message::Tick(_time) => {
                let start = Instant::now();

                match {
                    let mut video_frame_opt = self.flags.video_frame_lock.lock().unwrap();
                    video_frame_opt.take()
                } {
                    Some(video_frame) => {
                        let pts = video_frame.0.pts();
                        self.handle_opt = Some(video_frame.into_handle());

                        let duration = start.elapsed();
                        log::debug!(
                            "converted video frame at {:?} to handle in {:?}",
                            pts,
                            duration
                        );
                    }
                    None => {}
                }
            }
            Message::ToggleContextPage(context_page) => {
                //TODO: ensure context menus are closed
                if self.context_page == context_page {
                    self.core.window.show_context = !self.core.window.show_context;
                } else {
                    self.context_page = context_page;
                    self.core.window.show_context = true;
                }
                self.set_context_title(context_page.title());
            }
            Message::WindowClose => {
                return window::close(window::Id::MAIN);
            }
            Message::WindowNew => match env::current_exe() {
                Ok(exe) => match process::Command::new(&exe).spawn() {
                    Ok(_child) => {}
                    Err(err) => {
                        log::error!("failed to execute {:?}: {}", exe, err);
                    }
                },
                Err(err) => {
                    log::error!("failed to get current executable path: {}", err);
                }
            },
        }

        Command::none()
    }

    fn context_drawer(&self) -> Option<Element<Message>> {
        if !self.core.window.show_context {
            return None;
        }

        Some(match self.context_page {
            ContextPage::Settings => self.settings(),
        })
    }

    /// Creates a view after each update.
    fn view(&self) -> Element<Self::Message> {
        let content: Element<_> = match &self.handle_opt {
            Some(handle) => widget::image(handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => widget::text("Loading").into(),
        };

        // Uncomment to debug layout:
        //content.explain(cosmic::iced::Color::WHITE)
        content
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        struct ConfigSubscription;
        struct ThemeSubscription;

        Subscription::batch([
            window::frames().map(|(_window_id, instant)| Message::Tick(instant)),
            event::listen_with(|event, _status| match event {
                Event::Keyboard(KeyEvent::KeyPressed {
                    key_code,
                    modifiers,
                }) => Some(Message::Key(modifiers, key_code)),
                _ => None,
            }),
            cosmic_config::config_subscription(
                TypeId::of::<ConfigSubscription>(),
                Self::APP_ID.into(),
                CONFIG_VERSION,
            )
            .map(|update| {
                if !update.errors.is_empty() {
                    log::debug!("errors loading config: {:?}", update.errors);
                }
                Message::SystemThemeModeChange(update.config)
            }),
            cosmic_config::config_subscription::<_, cosmic_theme::ThemeMode>(
                TypeId::of::<ThemeSubscription>(),
                cosmic_theme::THEME_MODE_ID.into(),
                cosmic_theme::ThemeMode::version(),
            )
            .map(|update| {
                if !update.errors.is_empty() {
                    log::debug!("errors loading theme mode: {:?}", update.errors);
                }
                Message::SystemThemeModeChange(update.config)
            }),
        ])
    }
}
