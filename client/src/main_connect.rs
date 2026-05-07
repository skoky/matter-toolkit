use matc::discover;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Scanning for commissionable Matter devices on the LAN...");
    println!("This will take about 3 seconds...\n");

    // The matc discover module provides built-in functions to find devices.
    // We specify a timeout duration so the scanner doesn't block forever.
    let timeout = Duration::from_secs(3);

    // Discover commissionable devices (devices advertising _matterc._udp)
    let discovered_devices = discover::discover_commissionable(timeout).await?;

    if discovered_devices.is_empty() {
        println!("❌ No commissionable devices found. Make sure your device is in pairing mode.");
    } else {
        println!("✅ Found {} device(s):", discovered_devices.len());
        println!("--------------------------------------------------");

        for device in discovered_devices {
            // The resulting struct contains the parsed TXT records and network info
            println!("Name: {:?}", device.name);
            println!("IP Addresses: {:?}", device.ips);
            println!("IP Addresses: {:?}", device.source_ip);
            // println!("Port: {}", device.port);

            // Matter-specific metadata extracted from the mDNS broadcast
            println!("Discriminator: {:?}", device.discriminator);

            // Vendor and Product IDs are optional in the mDNS broadcast,
            // so we handle them as Options
            if let Some(vid) = device.vendor_id {
                println!("Vendor ID: {}", vid);
            }
            if let Some(pid) = device.product_id {
                println!("Product ID: {}", pid);
            }

            println!("--------------------------------------------------");
        }
    }

    Ok(())
}