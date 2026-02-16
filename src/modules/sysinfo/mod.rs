mod parser;
mod renderer;
mod token;

use crate::channels::{AsyncSenderExt, BroadcastReceiverExt};
use crate::clients::sysinfo::{Prefix, TokenType};
use crate::config::{CommonConfig, LayoutConfig, ModuleOrientation};
use crate::gtk_helpers::IronbarLabelExt;
use crate::modules::sysinfo::token::Part;
use crate::modules::{Module, ModuleInfo, ModuleParts, WidgetContext};
use crate::{clients, module_impl, spawn};
use color_eyre::Result;
use gtk::Label;
use gtk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

#[derive(Debug, Deserialize, Clone)]
#[cfg_attr(feature = "extras", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct SysInfoModule {
    /// List of strings including formatting tokens.
    /// For available tokens, see [below](#formatting-tokens).
    ///
    /// **Required**
    format: Vec<String>,

    /// Number of seconds between refresh.
    ///
    /// This can be set as a global interval,
    /// or passed as an object to customize the interval per-system.
    ///
    /// **Default**: `5`
    interval: Interval,

    /// The orientation by which the labels are laid out.
    ///
    /// **Valid options**: `horizontal`, `vertical`, `h`, `v`
    /// <br>
    /// **Default** : `horizontal`
    direction: Option<ModuleOrientation>,

    /// State thresholds for CSS classes.
    /// Map of class name to threshold value.
    /// The highest threshold not exceeding the metric value determines the active class.
    ///
    /// **Default**: `{}`
    #[serde(default)]
    states: BTreeMap<String, f64>,

    /// Which metric to evaluate for states.
    /// Auto-detected from interval config if not specified.
    /// Valid values: "cpu_percent", "memory_percent", etc.
    ///
    /// **Default**: auto-detect
    #[serde(default)]
    state_metric: Option<String>,

    /// Format string for tooltip shown on hover.
    /// Supports the same tokens as `format`.
    ///
    /// **Default**: none
    #[serde(default)]
    tooltip_format: Option<String>,

    // -- common --
    /// See [layout options](module-level-options#layout)
    #[serde(flatten)]
    layout: LayoutConfig,

    /// See [common options](module-level-options#common-options).
    #[serde(flatten)]
    pub common: Option<CommonConfig>,
}

impl Default for SysInfoModule {
    fn default() -> Self {
        Self {
            format: vec![],
            interval: Interval::default(),
            direction: None,
            states: BTreeMap::new(),
            state_metric: None,
            tooltip_format: None,
            layout: LayoutConfig::default(),
            common: Some(CommonConfig::default()),
        }
    }
}

#[derive(Debug, Deserialize, Copy, Clone)]
#[cfg_attr(feature = "extras", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct Intervals {
    /// The number of seconds between refreshing memory data.
    ///
    /// **Default**: `5`
    memory: u64,

    /// The number of seconds between refreshing CPU data.
    ///
    /// **Default**: `5`
    cpu: u64,

    /// The number of seconds between refreshing temperature data.
    ///
    /// **Default**: `5`
    temps: u64,

    /// The number of seconds between refreshing disk data.
    ///
    /// **Default**: `5`
    disks: u64,

    /// The number of seconds between refreshing network data.
    ///
    /// **Default**: `5`
    networks: u64,

    /// The number of seconds between refreshing system data.
    ///
    /// **Default**: `5`
    system: u64,
}

impl Default for Intervals {
    fn default() -> Self {
        Self {
            memory: 5,
            cpu: 5,
            temps: 5,
            disks: 5,
            networks: 5,
            system: 5,
        }
    }
}

#[derive(Debug, Deserialize, Copy, Clone)]
#[serde(untagged)]
#[cfg_attr(feature = "extras", derive(schemars::JsonSchema))]
pub enum Interval {
    All(u64),
    Individual(Intervals),
}

impl Default for Interval {
    fn default() -> Self {
        Self::All(5)
    }
}

impl Interval {
    const fn memory(self) -> u64 {
        match self {
            Self::All(n) => n,
            Self::Individual(intervals) => intervals.memory,
        }
    }

    const fn cpu(self) -> u64 {
        match self {
            Self::All(n) => n,
            Self::Individual(intervals) => intervals.cpu,
        }
    }

    const fn temps(self) -> u64 {
        match self {
            Self::All(n) => n,
            Self::Individual(intervals) => intervals.temps,
        }
    }

    pub const fn disks(self) -> u64 {
        match self {
            Self::All(n) => n,
            Self::Individual(intervals) => intervals.disks,
        }
    }

    pub const fn networks(self) -> u64 {
        match self {
            Self::All(n) => n,
            Self::Individual(intervals) => intervals.networks,
        }
    }

    const fn system(self) -> u64 {
        match self {
            Self::All(n) => n,
            Self::Individual(intervals) => intervals.system,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RefreshType {
    Memory,
    Cpu,
    Temps,
    Disks,
    Network,
    System,
}

impl TokenType {
    fn is_affected_by(self, refresh_type: RefreshType) -> bool {
        match self {
            Self::CpuFrequency | Self::CpuPercent => refresh_type == RefreshType::Cpu,
            Self::MemoryFree
            | Self::MemoryAvailable
            | Self::MemoryTotal
            | Self::MemoryUsed
            | Self::MemoryPercent
            | Self::SwapFree
            | Self::SwapTotal
            | Self::SwapUsed
            | Self::SwapPercent => refresh_type == RefreshType::Memory,
            Self::TempC | Self::TempF => refresh_type == RefreshType::Temps,
            Self::DiskFree
            | Self::DiskTotal
            | Self::DiskUsed
            | Self::DiskPercent
            | Self::DiskRead
            | Self::DiskWrite => refresh_type == RefreshType::Disks,
            Self::NetDown | Self::NetUp => refresh_type == RefreshType::Network,
            Self::LoadAverage1 | Self::LoadAverage5 | Self::LoadAverage15 => {
                refresh_type == RefreshType::System
            }
            Self::Uptime => refresh_type == RefreshType::System,
        }
    }
}

/// Given sorted states and a value, return the matching state class name.
fn resolve_state(states: &BTreeMap<String, f64>, value: f64) -> Option<String> {
    let mut result = None;
    for (name, &threshold) in states {
        if value >= threshold {
            result = Some(name.clone());
        }
    }
    result
}

/// Get the metric value for states evaluation.
fn get_state_value(
    client: &clients::sysinfo::Client,
    metric: &str,
    refresh: RefreshType,
) -> Option<f64> {
    let is_relevant = match metric {
        "cpu_percent" => refresh == RefreshType::Cpu,
        "memory_percent" => refresh == RefreshType::Memory,
        _ => false,
    };
    if !is_relevant {
        return None;
    }
    match metric {
        "cpu_percent" => {
            // cpu_percent returns ValueSet (per-CPU); compute mean
            use crate::clients::sysinfo::Function;
            Some(client.cpu_percent().apply(&Function::Mean, Prefix::None))
        }
        "memory_percent" => Some(client.memory_percent().get(Prefix::None)),
        _ => None,
    }
}

/// Message type: (label_index, rendered_text, optional_state_class, optional_tooltip)
type SysInfoUpdate = (usize, String, Option<String>, Option<String>);

impl Module<gtk::Box> for SysInfoModule {
    type SendMessage = SysInfoUpdate;
    type ReceiveMessage = ();

    module_impl!("sysinfo");

    fn spawn_controller(
        &self,
        _info: &ModuleInfo,
        context: &WidgetContext<Self::SendMessage, Self::ReceiveMessage>,
        _rx: mpsc::Receiver<Self::ReceiveMessage>,
    ) -> Result<()> {
        let interval = self.interval;

        let client = context.client::<clients::sysinfo::Client>();

        let format_tokens = self
            .format
            .iter()
            .map(|format| parser::parse_input(format.as_str()))
            .collect::<Result<Vec<_>>>()?;

        let tooltip_tokens = self
            .tooltip_format
            .as_ref()
            .map(|fmt| parser::parse_input(fmt.as_str()))
            .transpose()?;

        for (i, token_set) in format_tokens.iter().enumerate() {
            let rendered = Part::render_all(token_set, &client, interval);
            let tooltip = tooltip_tokens
                .as_ref()
                .map(|tt| Part::render_all(tt, &client, interval));
            context.tx.send_update_spawn((i, rendered, None, tooltip));
        }

        let (refresh_tx, mut refresh_rx) = mpsc::channel(16);

        macro_rules! spawn_refresh {
            ($refresh_type:expr, $func:ident) => {{
                let tx = refresh_tx.clone();
                spawn(async move {
                    loop {
                        tx.send_expect($refresh_type).await;
                        sleep(Duration::from_secs(interval.$func())).await;
                    }
                });
            }};
        }

        spawn_refresh!(RefreshType::Memory, memory);
        spawn_refresh!(RefreshType::Cpu, cpu);
        spawn_refresh!(RefreshType::Temps, temps);
        spawn_refresh!(RefreshType::Disks, disks);
        spawn_refresh!(RefreshType::Network, networks);
        spawn_refresh!(RefreshType::System, system);

        let tx = context.tx.clone();
        let states = self.states.clone();

        // Auto-detect state_metric from interval config if not specified
        let state_metric = self.state_metric.clone().unwrap_or_else(|| {
            if let Interval::Individual(ref intervals) = self.interval {
                if intervals.cpu != 5 { return "cpu_percent".to_string(); }
                if intervals.memory != 5 { return "memory_percent".to_string(); }
            }
            String::new()
        });

        spawn(async move {
            while let Some(refresh) = refresh_rx.recv().await {
                match refresh {
                    RefreshType::Memory => client.refresh_memory(),
                    RefreshType::Cpu => client.refresh_cpu(),
                    RefreshType::Temps => client.refresh_temps(),
                    RefreshType::Disks => client.refresh_disks(),
                    RefreshType::Network => client.refresh_network(),
                    RefreshType::System => client.refresh_load_average(),
                }

                let state_class = if !states.is_empty() && !state_metric.is_empty() {
                    get_state_value(&client, &state_metric, refresh)
                        .and_then(|v| resolve_state(&states, v))
                } else {
                    None
                };

                let has_state_update = state_class.is_some();
                let tooltip = tooltip_tokens
                    .as_ref()
                    .map(|tt| Part::render_all(tt, &client, interval));

                let has_tokens = format_tokens.iter().any(|ts| {
                    ts.iter().any(|p| matches!(p, Part::Token(_)))
                });

                for (i, token_set) in format_tokens.iter().enumerate() {
                    let is_affected = token_set
                        .iter()
                        .filter_map(|part| {
                            if let Part::Token(token) = part {
                                Some(token)
                            } else {
                                None
                            }
                        })
                        .any(|t| t.token.is_affected_by(refresh));

                    if is_affected || has_state_update || (!has_tokens && tooltip.is_some()) {
                        let rendered = Part::render_all(token_set, &client, interval);
                        tx.send_update((i, rendered, state_class.clone(), tooltip.clone())).await;
                    }
                }
            }
        });

        Ok(())
    }

    fn into_widget(
        self,
        context: WidgetContext<Self::SendMessage, Self::ReceiveMessage>,
        info: &ModuleInfo,
    ) -> Result<ModuleParts<gtk::Box>> {
        let layout = match self.direction {
            Some(orientation) => orientation.into(),
            None => self.layout.orientation(info),
        };

        let container = gtk::Box::new(layout, 10);

        let mut labels = Vec::new();

        for _ in &self.format {
            let label = Label::builder()
                .use_markup(true)
                .justify(self.layout.justify.into())
                .build();

            label.add_css_class("item");
            label.set_halign(gtk::Align::Center);
            label.set_valign(gtk::Align::Center);

            container.append(&label);
            labels.push(label);
        }

        let state_names: Vec<String> = self.states.keys().cloned().collect();

        context.subscribe().recv_glib((), move |(), data| {
            let (idx, text, state_class, tooltip) = data;
            let label = &labels[idx];
            label.set_label_escaped(&text);

            if let Some(ref tip) = tooltip {
                label.set_tooltip_text(Some(tip));
            }

            // Update state CSS classes on the label
            if let Some(ref class) = state_class {
                for name in &state_names {
                    if name == class {
                        label.add_css_class(name);
                    } else {
                        label.remove_css_class(name);
                    }
                }
            }
        });

        Ok(ModuleParts {
            widget: container,
            popup: None,
        })
    }
}
