//! Stable closed labels used by capture/report formats, independent of Display.

use kenwood_tmd750::RadioModel;
use kenwood_tmd750::memory::repeater_list::RepeaterListKind;
use kenwood_tmd750::memory::{
    TerminalGatewayMode, TerminalGatewayRoute, TerminalMode, TerminalUsbFunction, TerminalUsbRoute,
};

pub(crate) const fn model(value: RadioModel) -> &'static str {
    match value {
        RadioModel::TmD750 => "TM-D750",
    }
}

pub(crate) const fn usb_route(value: TerminalUsbRoute) -> &'static str {
    match value {
        TerminalUsbRoute::MainUnit => "USB (Main Unit)",
        TerminalUsbRoute::ControlPanel => "USB (Panel)",
    }
}

pub(crate) const fn usb_function(value: TerminalUsbFunction) -> &'static str {
    match value {
        TerminalUsbFunction::ComAndAudio => "COM+AF In/Out",
        TerminalUsbFunction::MassStorage => "Mass Storage",
    }
}

pub(crate) const fn gateway_route(value: TerminalGatewayRoute) -> &'static str {
    match value {
        TerminalGatewayRoute::MainUnit => "USB (Main Unit)",
        TerminalGatewayRoute::ControlPanel => "USB (Panel)",
        TerminalGatewayRoute::Bluetooth => "Bluetooth",
    }
}

pub(crate) const fn gateway_mode(value: TerminalGatewayMode) -> &'static str {
    match value {
        TerminalGatewayMode::Off => "Off",
        TerminalGatewayMode::Direct => "Direct Mode",
        TerminalGatewayMode::Terminal => "Terminal Mode",
    }
}

pub(crate) const fn terminal_mode(value: TerminalMode) -> &'static str {
    match value {
        TerminalMode::Reflector => "Reflector TERM Mode",
        TerminalMode::Repeater => "Repeater TERM Mode",
    }
}

pub(crate) const fn repeater_list(value: RepeaterListKind) -> &'static str {
    match value {
        RepeaterListKind::Repeater => "repeater",
        RepeaterListKind::Hotspot => "hotspot",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_labels_preserve_every_closed_spelling() {
        assert_eq!(model(RadioModel::TmD750), "TM-D750");
        assert_eq!(usb_route(TerminalUsbRoute::MainUnit), "USB (Main Unit)");
        assert_eq!(usb_route(TerminalUsbRoute::ControlPanel), "USB (Panel)");
        assert_eq!(
            usb_function(TerminalUsbFunction::ComAndAudio),
            "COM+AF In/Out"
        );
        assert_eq!(
            usb_function(TerminalUsbFunction::MassStorage),
            "Mass Storage"
        );
        assert_eq!(
            gateway_route(TerminalGatewayRoute::MainUnit),
            "USB (Main Unit)"
        );
        assert_eq!(
            gateway_route(TerminalGatewayRoute::ControlPanel),
            "USB (Panel)"
        );
        assert_eq!(gateway_route(TerminalGatewayRoute::Bluetooth), "Bluetooth");
        assert_eq!(gateway_mode(TerminalGatewayMode::Off), "Off");
        assert_eq!(gateway_mode(TerminalGatewayMode::Direct), "Direct Mode");
        assert_eq!(gateway_mode(TerminalGatewayMode::Terminal), "Terminal Mode");
        assert_eq!(
            terminal_mode(TerminalMode::Reflector),
            "Reflector TERM Mode"
        );
        assert_eq!(terminal_mode(TerminalMode::Repeater), "Repeater TERM Mode");
        assert_eq!(repeater_list(RepeaterListKind::Repeater), "repeater");
        assert_eq!(repeater_list(RepeaterListKind::Hotspot), "hotspot");
    }
}
