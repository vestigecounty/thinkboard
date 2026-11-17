//! Saying when a newer release is out.
//!
//! GitHub is asked for the repo's latest release at launch and once a day
//! while the app runs (`core::update`) — it runs for days, resident — and a
//! newer one is said in a corner of the board, quietly: its version, a link to
//! what's new, and a way to put it away, which holds for that release. Only
//! said: nothing is downloaded, and nothing replaces itself. Turned off in
//! Settings (`check_for_updates`); not asked at all inside a Flatpak, which
//! Flathub keeps up to date.

use iced::widget::{button, container, row, text};
use iced::{Element, Fill, Padding, Task};

use super::{App, Message, chrome};
use thinkboard_core::update::{self, Release};

/// How often a running app asks again.
pub(super) const EVERY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// This build's version.
pub(super) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How the app was installed, which says how it's updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Installed {
    /// From the release page, or built: the release page is the way.
    Download,
    /// By Homebrew, whose upgrade is the way.
    Homebrew,
    /// By Flatpak, which keeps it up to date itself.
    Flatpak,
}

impl Installed {
    pub(super) fn here() -> Self {
        if thinkboard_core::sandbox::flatpak_id().is_some() {
            return Installed::Flatpak;
        }
        let exe = std::env::current_exe().and_then(|exe| exe.canonicalize()).unwrap_or_default();
        Self::of(&exe)
    }

    /// A formula is installed under Homebrew's Cellar, wherever its prefix is.
    fn of(exe: &std::path::Path) -> Self {
        if exe.components().any(|part| part.as_os_str() == "Cellar") { Installed::Homebrew } else { Installed::Download }
    }
}

impl App {
    /// Asks GitHub for the latest release, where checking is on. Not under
    /// test: what's out on GitHub would decide what a test sees.
    pub(super) fn check_for_updates(&self) -> Task<Message> {
        if !self.config.check_for_updates || Installed::here() == Installed::Flatpak || cfg!(test) {
            return Task::none();
        }
        let agent = format!("ThinkBoard/{VERSION}");
        Task::perform(async move { update::latest(&agent).await }, Message::UpdateChecked)
    }

    /// GitHub's answer: kept where it's a later release than this one. A
    /// failed check says nothing — the next one is tomorrow.
    pub(super) fn update_checked(&mut self, answer: Result<Release, String>) {
        if let Ok(release) = answer {
            self.update = update::newer(&release.version, VERSION).then_some(release);
        }
    }

    /// The release to tell of: a later one, not put away, with checking on.
    pub(super) fn update_to_tell(&self) -> Option<&Release> {
        let release = self.update.as_ref()?;
        (self.config.check_for_updates && release.version != self.config.dismissed_update).then_some(release)
    }

    /// The notice put away: not said again for this release.
    pub(super) fn dismiss_update(&mut self) {
        if let Some(release) = &self.update {
            self.config.dismissed_update = release.version.clone();
            self.persist_config();
        }
    }

    /// The release's page, in the browser.
    pub(super) fn open_release(&self) {
        if let Some(release) = &self.update {
            crate::platform::open_url(&release.url);
        }
    }

    /// The notice, in the board's bottom-right corner: small, quiet, and out
    /// of the way of the cards — the version, what's new, and ×. Installed
    /// by Homebrew, it names the upgrade instead of a download. Square, on a
    /// wash of the accent: a rounded panel on the card colour read as one
    /// more card on the board.
    pub(super) fn update_notice(&self) -> Option<Element<'_, Message>> {
        let release = self.update_to_tell()?;
        let ct = self.theme;
        let font = ct.ui_font();
        let said = match Installed::here() {
            Installed::Homebrew => format!("ThinkBoard {} is out: brew upgrade thinkboard", release.version),
            _ => format!("ThinkBoard {} is out", release.version),
        };
        let link = |words: &'static str, press: Message| {
            button(text(words).size(13).font(font).color(ct.accent))
                .padding([2, 4])
                .on_press(press)
                .style(move |_theme, _status| button::Style::default())
        };
        let notice = row![
            text(said).size(13).font(font).color(ct.text),
            link("What's new", Message::OpenRelease),
            button(text("×").size(15).font(font).color(ct.text_muted))
                .padding([0, 4])
                .on_press(Message::DismissUpdate)
                .style(move |_theme, _status| button::Style::default()),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        let panel = container(notice).padding([6, 12]).style(move |_theme| container::Style {
            background: Some(chrome::mix(ct.background, ct.accent, 0.16).into()),
            border: iced::Border { radius: 0.0.into(), width: 1.0, color: iced::Color { a: 0.45, ..ct.accent } },
            text_color: Some(ct.text),
            ..container::Style::default()
        });
        Some(
            container(panel)
                .width(Fill)
                .height(Fill)
                .align_x(iced::alignment::Horizontal::Right)
                .align_y(iced::alignment::Vertical::Bottom)
                .padding(Padding { top: 0.0, right: 16.0, bottom: 16.0, left: 16.0 })
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_homebrew_install_is_told_by_its_cellar() {
        assert_eq!(Installed::of(std::path::Path::new("/opt/homebrew/Cellar/thinkboard/0.8.0/bin/thinkboard")), Installed::Homebrew);
        assert_eq!(Installed::of(std::path::Path::new("/home/linuxbrew/.linuxbrew/Cellar/thinkboard/0.8.0/bin/thinkboard")), Installed::Homebrew);
        assert_eq!(Installed::of(std::path::Path::new("/Applications/ThinkBoard.app/Contents/MacOS/thinkboard")), Installed::Download);
        assert_eq!(Installed::of(std::path::Path::new("/usr/bin/thinkboard")), Installed::Download);
    }
}
