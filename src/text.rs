//! Portuguese (pt-BR) labels for the technical strings drivers report.
//!
//! Drivers keep their protocol-level English names (they mirror the upstream sources); the UI
//! translates them here, in one place.

/// How a device is connected, e.g. "2.4 GHz receiver" -> "receptor 2,4 GHz".
pub fn via(via: &str) -> &str {
    match via {
        "2.4 GHz receiver" => "receptor 2,4 GHz",
        "2.4 GHz receiver / USB" | "2.4 GHz receiver or USB" => "receptor 2,4 GHz ou USB",
        "2.4 GHz dongle" => "dongle 2,4 GHz",
        "2.4 GHz dongle or USB" => "dongle 2,4 GHz ou USB",
        "USB cable" => "cabo USB",
        "USB base station" => "base USB",
        "UWB base station" => "base UWB",
        "Logitech receiver" => "receptor Logitech",
        "Razer receiver" => "receptor Razer",
        "Corsair receiver" => "receptor Corsair",
        "ROG Omni Receiver" => "receptor ROG Omni",
        "receiver" => "receptor",
        "Jabra Link dongle" => "dongle Jabra Link",
        other => other,
    }
}

/// Coarse battery level label, e.g. "Low" -> "Baixa".
pub fn level(level: &str) -> &str {
    match level {
        "Full" => "Cheia",
        "Good" => "Boa",
        "Medium" => "Média",
        "Low" => "Baixa",
        "Critical" => "Crítica",
        "Empty" => "Vazia",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_known_labels_and_keeps_names() {
        assert_eq!(via("USB cable"), "cabo USB");
        assert_eq!(via("Bluetooth"), "Bluetooth");
        assert_eq!(via("HeadsetControl"), "HeadsetControl");
        assert_eq!(level("Critical"), "Crítica");
        assert_eq!(level("50%"), "50%");
    }
}
