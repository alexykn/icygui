//! Which of a view's popup menus is open.
//!
//! Escape or a press outside an open menu closes it
//! (`ic_ui_kit::Dismissable`). When that press lands on the menu's own
//! trigger, the trigger's click must not open it again, so the press that
//! closed a menu is remembered and its click ignored.

use gpui::{ClickEvent, Pixels, Point};
use ic_ui_kit::Dismissal;

/// The open menu among those `T` names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenMenu<T> {
    open: Option<T>,
    dismissed: Option<(T, Point<Pixels>)>,
}

impl<T> Default for OpenMenu<T> {
    fn default() -> Self {
        Self {
            open: None,
            dismissed: None,
        }
    }
}

impl<T: Clone + PartialEq> OpenMenu<T> {
    /// The open menu.
    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<&T> {
        self.open.as_ref()
    }

    /// Whether `menu` is open.
    pub(crate) fn is_open(&self, menu: &T) -> bool {
        self.open.as_ref() == Some(menu)
    }

    /// A click on `menu`'s trigger that went down at `down`: opens it, or
    /// closes it if open. The press that just closed it is ignored.
    pub(crate) fn toggle(&mut self, menu: T, down: Option<Point<Pixels>>) {
        let dismissed = self.dismissed.take();
        if let (Some(down), Some((closed, at))) = (down, dismissed)
            && closed == menu
            && at == down
        {
            return;
        }
        self.open = if self.open.as_ref() == Some(&menu) {
            None
        } else {
            Some(menu)
        };
    }

    /// Opens `menu` (a right click).
    pub(crate) fn open(&mut self, menu: T) {
        self.dismissed = None;
        self.open = Some(menu);
    }

    /// A press at `at` outside the open menu.
    pub(crate) fn dismiss(&mut self, at: Point<Pixels>) {
        if let Some(menu) = self.open.take() {
            self.dismissed = Some((menu, at));
        }
    }

    /// The open menu closed by itself (`ic_ui_kit::Menu::on_dismiss`).
    pub(crate) fn dismissed(&mut self, how: Dismissal) {
        match how {
            Dismissal::Press(at) => self.dismiss(at),
            Dismissal::Escape => {
                self.close();
            }
        }
    }

    /// Closes the open menu. Returns whether one was open.
    pub(crate) fn close(&mut self) -> bool {
        self.dismissed = None;
        self.open.take().is_some()
    }
}

/// Where a mouse click went down (`None` for keyboard clicks).
pub(crate) fn down_position(event: &ClickEvent) -> Option<Point<Pixels>> {
    match event {
        ClickEvent::Mouse(click) => Some(click.down.position),
        ClickEvent::Keyboard(_) | ClickEvent::Touch(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

    use super::*;

    #[test]
    fn the_press_that_closed_a_menu_doesnt_reopen_it() {
        let mut menus = OpenMenu::default();
        menus.toggle("group", Some(point(px(1.), px(1.))));
        assert!(menus.is_open(&"group"));
        let press = point(px(150.), px(880.));
        menus.dismiss(press);
        assert_eq!(menus.current(), None);
        menus.toggle("group", Some(press));
        assert!(
            !menus.is_open(&"group"),
            "the press on the trigger closed it"
        );
        menus.toggle("group", Some(point(px(151.), px(880.))));
        assert!(menus.is_open(&"group"));
        // Another menu's trigger opens that one instead.
        menus.toggle("footer", None);
        assert!(menus.is_open(&"footer"));
        assert!(menus.close());
        assert!(!menus.close());
        menus.open("dashboard");
        assert_eq!(menus.current(), Some(&"dashboard"));
    }

    #[test]
    fn a_press_closing_one_menu_may_open_another() {
        let mut menus = OpenMenu::default();
        menus.open("group-a");
        let press = point(px(10.), px(10.));
        menus.dismiss(press);
        menus.toggle("group-b", Some(press));
        assert!(menus.is_open(&"group-b"));
    }
}
