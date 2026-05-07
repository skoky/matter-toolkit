use mdns_sd::{ServiceDaemon, ServiceEvent};

fn main() {
    // Create an mDNS service daemon
    let mdns = ServiceDaemon::new().expect("Failed to create mDNS daemon");

    // The standard Matter service type for commissionable devices
    let service_type = "_matterc._udp.local.";

    // Start browsing for the service
    let receiver = mdns.browse(service_type).expect("Failed to browse for Matter services");

    println!("Scanning for Matter commissionable devices on the local network...");
    println!("(Press Ctrl+C to stop)\n");

    // Block and listen for incoming mDNS events
    while let Ok(event) = receiver.recv() {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                println!("✅ Found Commissionable Matter Device!");
                println!("  Name: {}", info.get_fullname());
                println!("  IP Addresses: {:?}", info.get_addresses());
                println!("  Port: {}", info.get_port());

                // Matter devices broadcast metadata in their TXT records
                // Common keys include:
                // D: Discriminator
                // V: Vendor ID
                // P: Product ID
                // CM: Commissioning Mode (1 or 2)
                println!("  Matter TXT Records:");
                for property in info.get_properties().iter() {
                    let key = property.key();
                    let val = property.val_str();

                    let description = match key {
                        "D" => " (Discriminator)",
                        "V" => " (Vendor ID)",
                        "P" => " (Product ID)",
                        "CM" => " (Commissioning Mode)",
                        "CRA" => " (Commissioning Rotating ID)",
                        "T" => " (TCP Supported)",
                        _ => "",
                    };

                    println!("    - {}: {}{}", key, val, description);
                }
                println!("--------------------------------------------------");
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                println!("❌ Device disconnected or stopped advertising: {}", fullname);
            }
            _ => {}
        }
    }
}