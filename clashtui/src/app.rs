use core::cell::{OnceCell, RefCell};
use std::{path::PathBuf, rc::Rc};

use crate::msgpopup_methods;
use crate::tui::{
    tabs::{ClashSrvCtlTab, ProfileTab, TabEvent, Tabs},
    tools,
    utils::{HelpPopUp, InfoPopUp, Keys},
    widgets::MsgPopup,
    EventState, StatusBar, TabBar, Theme, Visibility,
};
use crate::utils::{
    CfgError, ClashTuiUtil, Flag, Flags, SharedClashTuiState, SharedClashTuiUtil, State,
};

pub struct App {
    tabbar: TabBar,
    tabs: Vec<Tabs>,
    pub should_quit: bool,
    help_popup: OnceCell<Box<HelpPopUp>>,
    info_popup: InfoPopUp,
    msgpopup: MsgPopup,

    clashtui_util: SharedClashTuiUtil,
    statusbar: StatusBar,
}

impl App {
    pub fn new(flags: &Flags<Flag>, clashtui_config_dir: &PathBuf) -> (Option<Self>, Vec<CfgError>) {
        #[cfg(debug_assertions)]
        let _ = std::fs::remove_file(clashtui_config_dir.join("clashtui.log")); // auto rm old log for debug
        setup_logging(clashtui_config_dir.join("clashtui.log").to_str().unwrap());

        let (util, err_track) =
            ClashTuiUtil::new(clashtui_config_dir, !flags.contains(Flag::FirstInit));
        if flags.contains(Flag::UpdateOnly) {
            log::info!("Cron Mode!");
            util.get_profile_names()
                .unwrap()
                .into_iter()
                .inspect(|s| println!("\nProfile: {s}"))
                .filter_map(|v| {
                    util.update_local_profile(&v, false)
                        .map_err(|e| println!("- Error! {e}"))
                        .ok()
                })
                .flatten()
                .for_each(|s| println!("- {s}"));

            return (None, err_track);
        } // Finish cron
        let clashtui_util = SharedClashTuiUtil::new(util);

        let clashtui_state =
            SharedClashTuiState::new(RefCell::new(State::new(Rc::clone(&clashtui_util))));
        let _ = Theme::load(None).map_err(|e| log::error!("Loading Theme:{e}"));

        let tabs: Vec<Tabs> = vec![
            Tabs::Profile(ProfileTab::new(
                clashtui_util.clone(),
                clashtui_state.clone(),
            )),
            Tabs::ClashSrvCtl(ClashSrvCtlTab::new(
                clashtui_util.clone(),
                clashtui_state.clone(),
            )),
        ]; // Init the tabs
        let tabbar = TabBar::new(tabs.iter().map(|v| v.to_string()).collect());
        let statusbar = StatusBar::new(Rc::clone(&clashtui_state));
        let info_popup = InfoPopUp::with_items(&clashtui_util.clash_version());

        let app = Self {
            tabbar,
            should_quit: false,
            help_popup: Default::default(),
            info_popup,
            msgpopup: Default::default(),
            statusbar,
            clashtui_util,
            tabs,
        };

        (Some(app), err_track)
    }

    fn popup_event(&mut self, ev: &crossterm::event::Event) -> Result<EventState, ui::Infailable> {
        // ## Self Popups
        let mut event_state = self
            .help_popup
            .get_mut()
            .and_then(|v| v.event(ev).ok())
            .unwrap_or(EventState::NotConsumed);
        if event_state.is_notconsumed() {
            event_state = self.info_popup.event(ev)?;
        }
        // ## Tab Popups
        let mut iter = self.tabs.iter_mut().map(|v| match v {
            Tabs::Profile(tab) => tab.popup_event(ev),
            Tabs::ClashSrvCtl(tab) => tab.popup_event(ev),
        });
        while event_state.is_notconsumed() {
            match iter.next() {
                Some(v) => event_state = v?,
                None => break,
            }
        }

        Ok(event_state)
    }

    pub fn event(&mut self, ev: &crossterm::event::Event) -> Result<EventState, std::io::Error> {
        let mut event_state = self.msgpopup.event(ev)?;
        if event_state.is_notconsumed() {
            event_state = self.popup_event(ev)?;
        }
        if event_state.is_consumed() {
            return Ok(event_state);
        }

        if let crossterm::event::Event::Key(key) = ev {
            if key.kind != crossterm::event::KeyEventKind::Press {
                return Ok(EventState::NotConsumed);
            }
            event_state = match key.code.into() {
                Keys::AppQuit => {
                    self.should_quit = true;
                    EventState::WorkDone
                }
                Keys::AppHelp => {
                    self.help_popup.get_or_init(|| Box::new(HelpPopUp::new()));
                    self.help_popup.get_mut().unwrap().show();
                    EventState::WorkDone
                }
                Keys::AppInfo => {
                    self.info_popup.show();
                    EventState::WorkDone
                }
                Keys::ClashConfig => {
                    let _ = self
                        .clashtui_util
                        .open_dir(self.clashtui_util.clashtui_dir.as_path())
                        .map_err(|e| log::error!("ODIR: {}", e));
                    EventState::WorkDone
                }
                Keys::AppConfig => {
                    let _ = self
                        .clashtui_util
                        .open_dir(&PathBuf::from(&self.clashtui_util.tui_cfg.clash_cfg_dir))
                        .map_err(|e| log::error!("ODIR: {}", e));
                    EventState::WorkDone
                }
                Keys::LogCat => {
                    let log = self.clashtui_util.fetch_recent_logs(20);
                    self.popup_list_msg(log);
                    EventState::WorkDone
                }
                Keys::SoftRestart => {
                    match self.clashtui_util.restart_clash() {
                        Ok(output) => {
                            self.popup_list_msg(output.lines().map(|line| line.trim().to_string()));
                        }
                        Err(err) => {
                            self.popup_txt_msg(err.to_string());
                        }
                    }
                    EventState::WorkDone
                }
                _ => EventState::NotConsumed,
            };

            if event_state == EventState::NotConsumed {
                event_state = self
                    .tabbar
                    .event(ev)
                    .map_err(|()| std::io::Error::new(std::io::ErrorKind::Other, "Undefined"))?;
                let mut iter = self.tabs.iter_mut().map(|v| match v {
                    Tabs::Profile(tab) => tab.event(ev),
                    Tabs::ClashSrvCtl(tab) => Ok(tab.event(ev)?),
                });
                while event_state.is_notconsumed() {
                    match iter.next() {
                        Some(v) => event_state = v?,
                        None => break,
                    }
                }
            }
        }

        Ok(event_state)
    }
    fn late_event(&mut self) {
        self.tabs.iter_mut().for_each(|v| match v {
            Tabs::Profile(tab) => tab.late_event(),
            Tabs::ClashSrvCtl(tab) => tab.late_event(),
        })
    }
    // For refreshing the interface before performing lengthy operation.
    pub fn handle_last_ev(&mut self, last_ev: &EventState) -> EventState {
        self.late_event();
        match last_ev {
            EventState::NotConsumed | EventState::WorkDone => EventState::NotConsumed,
            EventState::Yes | EventState::Cancel => unreachable!(),
        }
    }

    pub fn draw(&mut self, f: &mut ratatui::prelude::Frame) {
        use ratatui::prelude as Ra;
        let chunks = Ra::Layout::default()
            .constraints(
                [
                    Ra::Constraint::Length(3),
                    Ra::Constraint::Min(0),
                    Ra::Constraint::Length(3),
                ]
                .as_ref(),
            )
            .split(f.size());

        self.update_tabbar();
        self.tabbar.draw(f, chunks[0]);

        let tab_chunk = chunks[1];
        self.tabs.iter_mut().for_each(|v| match v {
            Tabs::Profile(tab) => tab.draw(f, tab_chunk),
            Tabs::ClashSrvCtl(tab) => tab.draw(f, tab_chunk),
        });

        self.statusbar.draw(f, chunks[2]);

        let help_area = tools::centered_percent_rect(60, 60, f.size());
        if let Some(v) = self.help_popup.get_mut() {
            v.draw(f, help_area)
        }
        self.info_popup.draw(f, help_area);
        self.msgpopup.draw(f, help_area);
    }

    pub fn on_tick(&mut self) {}

    fn update_tabbar(&mut self) {
        let tabname = self
            .tabbar
            .selected()
            .expect("UB: selected tab out of bound");
        self.tabs
            .iter_mut()
            .map(|v| (v == tabname, v))
            .for_each(|(b, v)| match v {
                Tabs::Profile(tab) => tab.set_visible(b),
                Tabs::ClashSrvCtl(tab) => tab.set_visible(b),
            });
    }

    pub fn save(&self, config_path: &str) -> std::io::Result<()> {
        self.clashtui_util
            .tui_cfg
            .to_file(config_path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}

fn setup_logging(log_path: &str) {
    use log4rs::append::file::FileAppender;
    use log4rs::config::{Appender, Config, Root};
    use log4rs::encode::pattern::PatternEncoder;
    let mut flag = false;
    if let Ok(m) = std::fs::File::open(log_path).and_then(|f| f.metadata()) {
        if m.len() > 1024 * 1024 {
            let _ = std::fs::remove_file(log_path);
            flag = true
        };
    }
    // No need to change. This is set to auto switch to Info level when build release
    #[allow(unused_variables)]
    let log_level = log::LevelFilter::Info;
    #[cfg(debug_assertions)]
    let log_level = log::LevelFilter::Debug;
    let file_appender = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new("[{l}] {t} - {m}{n}")))
        .build(log_path)
        .unwrap();

    let config = Config::builder()
        .appender(Appender::builder().build("file", Box::new(file_appender)))
        .build(Root::builder().appender("file").build(log_level))
        .unwrap();

    log4rs::init_config(config).unwrap();
    if flag {
        log::info!("Log file too large, clear")
    }
    log::info!("Start Log, level: {}", log_level);
}

msgpopup_methods!(App);
