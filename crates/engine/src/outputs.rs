//! Show and audition outputs (thorncue-spec#9, ARCHITECTURE.md "Output devices").
//!
//! The rule: **an audition never plays through the show output unless the operator chose
//! that on purpose.** Everything here is pure (no cpal), so the rules are tested without a
//! sound card; `list_devices` is the only part that asks the system.

use serde::{Deserialize, Serialize};

/// An output device as the operator sees it. `id` is cpal's stable id (`host:id`); `name` is
/// what the system calls it. Saved choices match by id first, then by name (ids can change
/// when a device is plugged into another port).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
}

/// The devices present now, and which one the system default is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DeviceList {
    pub devices: Vec<OutputDevice>,
    /// Id of the system default output, if there is one.
    pub default: Option<String>,
}

impl DeviceList {
    /// A saved device, matched by id, else by name.
    pub fn find(&self, saved: &OutputDevice) -> Option<&OutputDevice> {
        self.devices
            .iter()
            .find(|d| d.id == saved.id)
            .or_else(|| self.devices.iter().find(|d| d.name == saved.name))
    }

    fn default_device(&self) -> Option<&OutputDevice> {
        let id = self.default.as_ref()?;
        self.devices.iter().find(|d| &d.id == id)
    }
}

/// Where GO plays.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShowOutput {
    #[default]
    SystemDefault,
    Device(OutputDevice),
}

/// Where auditions play. `NotChosen` is the default on a fresh install, and blocks them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditionOutput {
    #[default]
    NotChosen,
    /// "Same as show output (the audience hears it)": chosen on purpose, after a warning.
    SameAsShow,
    Device(OutputDevice),
}

/// What the show output resolves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShowRoute {
    /// The device to open; `None` = the system default.
    pub device: Option<OutputDevice>,
    /// Name to show in the bar.
    pub name: String,
    /// The chosen device isn't here, so the show plays on the system default (never block GO).
    pub fallback: bool,
}

/// Why auditions can't play right now. Each is a sentence for the bar and the toast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum AuditionBlock {
    /// Rule 1: nothing chosen yet.
    NotChosen,
    /// Rule 2: the chosen device isn't connected.
    Missing(String),
    /// Rule 3: it's the show's own device (or might be).
    SameDevice(String),
    /// Rule 4: the stream wouldn't open, or failed.
    Failed(String),
}

impl AuditionBlock {
    pub fn message(&self) -> String {
        match self {
            Self::NotChosen => "Choose an audition output (headphones) to audition cues.".into(),
            Self::Missing(name) => {
                format!("Audition output \"{name}\" isn't connected. Plug it in or choose another.")
            }
            Self::SameDevice(name) => format!(
                "The audition output \"{name}\" is, or may be, the show output, so the audience could hear auditions. Choose another output."
            ),
            Self::Failed(why) => format!("The audition output couldn't be used: {why}"),
        }
    }
}

/// What the audition output resolves to, when it isn't blocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditionRoute {
    /// The device to open; `None` = the system default (only when the operator chose "Same as
    /// show output" and the show is on the default).
    pub device: Option<OutputDevice>,
    pub name: String,
    /// Plays through the show output: the bar keeps a standing warning.
    pub through_show: bool,
}

/// Which physical output a device id stands for, as far as we can tell. Two devices with the
/// same key are the same speakers. ALSA lists one card many times (`hw:`, `plughw:`,
/// `front:`, `sysdefault:` ...) and routes `default`/`pulse`/`pipewire` through the system
/// mixer to *some* card, so those can't be told apart from any card.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Physical {
    Exact(String),
    AlsaCard(String),
    /// Goes through the system mixer to a card we can't name.
    Mixer,
}

fn physical(id: &str) -> Physical {
    let Some(pcm) = id.strip_prefix("alsa:") else {
        return Physical::Exact(id.to_string());
    };
    if let Some(i) = pcm.find("CARD=") {
        let card = pcm[i + 5..].split([',', ':']).next().unwrap_or("");
        return Physical::AlsaCard(card.to_string());
    }
    for prefix in ["hw:", "plughw:"] {
        if let Some(rest) = pcm.strip_prefix(prefix) {
            let card = rest.split([',', ':']).next().unwrap_or("");
            return Physical::AlsaCard(card.to_string());
        }
    }
    Physical::Mixer
}

/// Could these two be the same speakers? When we can't tell, yes (thorncue-spec#9 rule 3).
fn maybe_same(a: &OutputDevice, b: &OutputDevice) -> bool {
    if a.id == b.id || a.name == b.name {
        return true;
    }
    match (physical(&a.id), physical(&b.id)) {
        (Physical::Exact(x), Physical::Exact(y)) => x == y,
        (Physical::AlsaCard(x), Physical::AlsaCard(y)) => x == y,
        (Physical::Exact(_), Physical::AlsaCard(_))
        | (Physical::AlsaCard(_), Physical::Exact(_)) => false,
        // The mixer could be routed to anything.
        _ => true,
    }
}

/// Resolve the show output. A missing device falls back to the system default with a
/// warning: the show must keep playing (CLAUDE.md rule 2).
pub fn show_route(choice: &ShowOutput, list: &DeviceList) -> ShowRoute {
    let default_name = || {
        list.default_device()
            .map(|d| format!("System default ({})", d.name))
            .unwrap_or_else(|| "System default".into())
    };
    match choice {
        ShowOutput::SystemDefault => ShowRoute {
            device: None,
            name: default_name(),
            fallback: false,
        },
        ShowOutput::Device(saved) => match list.find(saved) {
            Some(d) => ShowRoute {
                device: Some(d.clone()),
                name: d.name.clone(),
                fallback: false,
            },
            None => ShowRoute {
                device: None,
                name: default_name(),
                fallback: true,
            },
        },
    }
}

/// Resolve the audition output against the show's (rules 1–3; rule 4 is found on opening).
pub fn audition_route(
    choice: &AuditionOutput,
    show: &ShowRoute,
    list: &DeviceList,
) -> Result<AuditionRoute, AuditionBlock> {
    match choice {
        AuditionOutput::NotChosen => Err(AuditionBlock::NotChosen),
        AuditionOutput::SameAsShow => Ok(AuditionRoute {
            device: show.device.clone(),
            name: show.name.clone(),
            through_show: true,
        }),
        AuditionOutput::Device(saved) => {
            let Some(d) = list.find(saved) else {
                return Err(AuditionBlock::Missing(saved.name.clone()));
            };
            // The show's real device: the chosen one, or what the system default is now.
            let show_device = match &show.device {
                Some(s) => Some(s.clone()),
                None => list.default_device().cloned(),
            };
            match show_device {
                Some(s) if !maybe_same(d, &s) => Ok(AuditionRoute {
                    device: Some(d.clone()),
                    name: d.name.clone(),
                    through_show: false,
                }),
                // Same device, or no way to know what the default is: treat as the same.
                _ => Err(AuditionBlock::SameDevice(d.name.clone())),
            }
        }
    }
}

/// Ask the system which outputs exist. Slow on some systems (ALSA probes plugins), so never
/// call it on the engine thread: the device watcher runs it on its own thread.
pub fn list_devices() -> DeviceList {
    use kira::backend::cpal::cpal::traits::{DeviceTrait, HostTrait};
    let host = kira::backend::cpal::cpal::default_host();
    let id_of = |d: &kira::backend::cpal::cpal::Device| d.id().ok().map(|id| id.to_string());
    let default = host.default_output_device().as_ref().and_then(id_of);
    let mut devices = Vec::new();
    match host.output_devices() {
        Ok(it) => {
            for d in it {
                let Some(id) = id_of(&d) else { continue };
                if !is_operator_output(&id) {
                    continue;
                }
                let name = d
                    .description()
                    .map(|desc| desc.name().to_string())
                    .unwrap_or_else(|_| id.clone());
                if !devices.iter().any(|x: &OutputDevice| x.id == id) {
                    devices.push(OutputDevice { id, name });
                }
            }
        }
        Err(e) => tracing::warn!("listing output devices: {e}"),
    }
    DeviceList { devices, default }
}

/// ALSA lists its converter and effect plugins as outputs. They mean nothing to an operator
/// (and `null` throws the sound away), so they're left out of the lists.
fn is_operator_output(id: &str) -> bool {
    const ALSA_PLUGINS: [&str; 7] = [
        "null",
        "lavrate",
        "samplerate",
        "speexrate",
        "speex",
        "upmix",
        "vdownmix",
    ];
    match id.strip_prefix("alsa:") {
        Some(pcm) => !ALSA_PLUGINS.contains(&pcm),
        None => true,
    }
}

/// The cpal device for an id, for opening a stream.
pub(crate) fn cpal_device(id: &str) -> Option<kira::backend::cpal::cpal::Device> {
    use kira::backend::cpal::cpal::traits::HostTrait;
    let id: kira::backend::cpal::cpal::DeviceId = id.parse().ok()?;
    kira::backend::cpal::cpal::default_host().device_by_id(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str) -> OutputDevice {
        OutputDevice {
            id: id.into(),
            name: name.into(),
        }
    }

    fn mac() -> DeviceList {
        DeviceList {
            devices: vec![
                dev("coreaudio:BuiltIn", "MacBook Speakers"),
                dev("coreaudio:Focusrite", "Scarlett 2i2"),
                dev("coreaudio:AirPods", "AirPods"),
            ],
            default: Some("coreaudio:BuiltIn".into()),
        }
    }

    #[test]
    fn a_saved_device_is_found_by_id_then_by_name() {
        let list = mac();
        assert_eq!(
            list.find(&dev("coreaudio:Focusrite", "old name"))
                .unwrap()
                .name,
            "Scarlett 2i2"
        );
        // The id changed (another USB port): the name still finds it.
        assert_eq!(
            list.find(&dev("coreaudio:Focusrite-2", "Scarlett 2i2"))
                .unwrap()
                .id,
            "coreaudio:Focusrite"
        );
        assert!(list.find(&dev("x", "Gone")).is_none());
    }

    #[test]
    fn show_falls_back_to_the_default_when_its_device_is_gone() {
        let r = show_route(&ShowOutput::Device(dev("coreaudio:X", "Desk")), &mac());
        assert!(r.fallback);
        assert_eq!(r.device, None);
        assert_eq!(r.name, "System default (MacBook Speakers)");
    }

    #[test]
    fn rule_1_nothing_chosen_blocks() {
        let show = show_route(&ShowOutput::SystemDefault, &mac());
        assert_eq!(
            audition_route(&AuditionOutput::NotChosen, &show, &mac()),
            Err(AuditionBlock::NotChosen)
        );
    }

    #[test]
    fn rule_2_a_missing_device_blocks_and_is_never_rerouted() {
        let show = show_route(&ShowOutput::SystemDefault, &mac());
        assert_eq!(
            audition_route(
                &AuditionOutput::Device(dev("coreaudio:Phones", "Headphones")),
                &show,
                &mac()
            ),
            Err(AuditionBlock::Missing("Headphones".into()))
        );
    }

    #[test]
    fn rule_3_the_show_device_blocks_including_via_system_default() {
        let list = mac();
        let focusrite = dev("coreaudio:Focusrite", "Scarlett 2i2");
        let show = show_route(&ShowOutput::Device(focusrite.clone()), &list);
        assert!(matches!(
            audition_route(&AuditionOutput::Device(focusrite), &show, &list),
            Err(AuditionBlock::SameDevice(_))
        ));
        // Show on "System default", which is the built-in speakers right now.
        let show = show_route(&ShowOutput::SystemDefault, &list);
        assert!(matches!(
            audition_route(
                &AuditionOutput::Device(dev("coreaudio:BuiltIn", "MacBook Speakers")),
                &show,
                &list
            ),
            Err(AuditionBlock::SameDevice(_))
        ));
        let ok = audition_route(
            &AuditionOutput::Device(dev("coreaudio:AirPods", "AirPods")),
            &show,
            &list,
        )
        .unwrap();
        assert!(!ok.through_show);
        assert_eq!(ok.name, "AirPods");
    }

    #[test]
    fn rule_3_when_we_cant_tell_its_the_same() {
        let list = DeviceList {
            devices: vec![
                dev("alsa:default", "Default"),
                dev("alsa:pipewire", "PipeWire"),
                dev("alsa:hw:CARD=PCH,DEV=0", "HDA Intel PCH"),
                dev("alsa:front:CARD=PCH,DEV=0", "HDA Intel PCH, front"),
                dev("alsa:hw:CARD=Headset,DEV=0", "USB Headset"),
            ],
            default: Some("alsa:default".into()),
        };
        let show = show_route(&ShowOutput::SystemDefault, &list);
        // The default goes through the mixer: it could be any card.
        for d in &list.devices[1..] {
            assert!(
                audition_route(&AuditionOutput::Device(d.clone()), &show, &list).is_err(),
                "{d:?}"
            );
        }
        // Show on the PCH card: the same card under another name is the same; another card
        // isn't; the mixer might be routed to PCH.
        let show = show_route(&ShowOutput::Device(list.devices[2].clone()), &list);
        let route = |i: usize| {
            audition_route(
                &AuditionOutput::Device(list.devices[i].clone()),
                &show,
                &list,
            )
        };
        assert!(route(3).is_err());
        assert!(route(1).is_err());
        assert!(route(4).is_ok());
        // No default at all: can't tell.
        let none = DeviceList {
            default: None,
            ..mac()
        };
        let show = show_route(&ShowOutput::SystemDefault, &none);
        assert!(
            audition_route(
                &AuditionOutput::Device(dev("coreaudio:AirPods", "AirPods")),
                &show,
                &none
            )
            .is_err()
        );
    }

    #[test]
    fn same_as_show_is_allowed_only_as_a_choice_and_says_so() {
        let show = show_route(&ShowOutput::SystemDefault, &mac());
        let r = audition_route(&AuditionOutput::SameAsShow, &show, &mac()).unwrap();
        assert!(r.through_show);
    }

    #[test]
    fn alsa_plugins_arent_offered_as_outputs() {
        assert!(!is_operator_output("alsa:samplerate"));
        assert!(!is_operator_output("alsa:null"));
        assert!(is_operator_output("alsa:default"));
        assert!(is_operator_output("alsa:hw:CARD=Headset,DEV=0"));
        assert!(is_operator_output("coreaudio:BuiltIn"));
    }

    #[test]
    fn saved_choices_round_trip_through_json() {
        let a = AuditionOutput::Device(dev("coreaudio:AirPods", "AirPods"));
        let s = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<AuditionOutput>(&s).unwrap(), a);
        let s = serde_json::to_string(&ShowOutput::SystemDefault).unwrap();
        assert_eq!(s, r#"{"kind":"system_default"}"#);
    }
}
