use super::open_state::OpenState;
use crate::channels::AsyncSenderExt;
use crate::gtk_helpers::IronbarLabelExt;
use crate::image;
use crate::image::IconButton;
use crate::modules::workspaces::WorkspaceItemContext;
use glib::signal::SignalHandlerId;
use gtk::prelude::*;
use gtk::Button as GtkButton;
use gtk::Orientation;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct Button {
    button: GtkButton,
    label: gtk::Label,
    icon_container: Option<gtk::Box>,
    image_provider: image::Provider,
    icon_size: i32,
    workspace_id: i64,
    monitor: String,
    open_state: OpenState,
    conn_id: Option<SignalHandlerId>,
    tx: mpsc::Sender<i64>,
}

impl Button {
    pub fn new(
        id: i64,
        index: i64,
        name: &str,
        monitor: &str,
        open_state: OpenState,
        context: &WorkspaceItemContext,
    ) -> Self {
        let label_text = context.format_label(name, index);

        let icon_button =
            IconButton::new(&label_text, context.icon_size, &context.image_provider);

        let button: GtkButton = (*icon_button).clone();
        let label = icon_button.label().clone();

        button.set_widget_name(name);
        button.add_css_class("item");
        button.set_valign(gtk::Align::Center);
        button.set_vexpand(false);
        label.set_valign(gtk::Align::Center);
        label.set_halign(gtk::Align::Center);
        label.set_size_request(22, 22);
        label.set_vexpand(false);

        let tx = context.tx.clone();

        let conn_id = button.connect_clicked(move |_item| {
            tx.send_spawn(id);
        });

        let btn = Self {
            button,
            label,
            icon_container: None,
            image_provider: context.image_provider.clone(),
            icon_size: 14,
            workspace_id: id,
            monitor: monitor.to_string(),
            open_state,
            conn_id: Some(conn_id),
            tx: context.tx.clone(),
        };

        btn.apply_open_state();
        btn
    }

    fn ensure_icon_container(&mut self) {
        if self.icon_container.is_some() {
            return;
        }

        let container = gtk::Box::new(Orientation::Horizontal, 0);
        container.add_css_class("button-contents");

        let icon_box = gtk::Box::new(Orientation::Horizontal, 2);
        icon_box.add_css_class("window-icons");

        // Unparent label from button before re-parenting into container
        self.button.set_child(None::<&gtk::Widget>);

        container.append(&self.label);
        container.append(&icon_box);

        self.button.set_child(Some(&container));
        self.icon_container = Some(icon_box);
    }

    pub fn set_window_icons(&mut self, classes: &[String]) {
        self.ensure_icon_container();

        let icon_container = self.icon_container.as_ref().unwrap();

        while let Some(child) = icon_container.first_child() {
            icon_container.remove(&child);
        }

        let scale = icon_container.scale_factor();
        for class in classes {
            let paintable =
                self.image_provider
                    .lookup_icon(class, self.icon_size, scale);
            let image = gtk::Image::from_paintable(Some(&paintable));
            image.set_pixel_size(self.icon_size);
            image.add_css_class("window-icon");
            icon_container.append(&image);
        }
    }

    pub fn button(&self) -> &GtkButton {
        &self.button
    }

    pub fn set_label(&self, label: &str) {
        self.label.set_label_escaped(label);
    }

    pub fn open_state(&self) -> OpenState {
        self.open_state
    }

    pub fn set_open_state(&mut self, open_state: OpenState) {
        if self.open_state == open_state {
            return;
        }
        self.open_state = open_state;
        self.apply_open_state();
    }

    fn apply_open_state(&self) {
        let open_state = self.open_state;

        if open_state.is_visible() {
            self.button.add_css_class("visible");
        } else {
            self.button.remove_css_class("visible");
        }

        if open_state == OpenState::Focused {
            self.button.add_css_class("focused");
        } else {
            self.button.remove_css_class("focused");
        }

        if open_state == OpenState::Closed {
            self.button.add_css_class("inactive");
        } else {
            self.button.remove_css_class("inactive");
        }
    }

    pub fn set_urgent(&self, urgent: bool) {
        if urgent {
            self.button.add_css_class("urgent");
        } else {
            self.button.remove_css_class("urgent");
        }
    }

    pub fn workspace_id(&self) -> i64 {
        self.workspace_id
    }

    pub fn set_workspace_id(&mut self, id: i64) {
        self.workspace_id = id;
        if let Some(conn_id) = self.conn_id.take() {
            self.button.disconnect(conn_id);
        }
        let tx = self.tx.clone();
        let conn_id = self.button.connect_clicked(move |_item| {
            tx.send_spawn(id);
        });
        self.conn_id = Some(conn_id);
    }

    pub fn monitor(&self) -> &str {
        &self.monitor
    }

    pub fn set_monitor(&mut self, monitor: &str) {
        self.monitor = monitor.to_string();
    }
}
