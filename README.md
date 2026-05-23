# Matter Example in Rust

This repository contains two example applications written in Rust. The `bridge` app demonstrates a CSA Matter bridge application with 1 or 2 connected lights. The `client` app demonstrates a CSA Matter client application. The bridge app can be paired or commissioned with any Matter-certified controller (or client), such as Alexa or Siri. Please review the documentation for those controllers before commissioning, as they might require an additional hardware component, like a HomePod, to work.

Commissioning is currently only implemented for devices already connected to the same Wi-Fi network. Bluetooth commissioning is TBD.

For terminology and more information, see the [CSA's Matter website](https://csa-iot.org/all-solutions/matter/). The code uses a Rust-based implementation of the protocol, utilizing the crate `rs-matter` for the bridge and `mattc` for the client.

Both the bridge and client have only been tested on Ubuntu Linux (x86-64) and Raspberry Pi (arm64). The OS is important here because of the implementation of mDNS, which Matter uses for broadcasting commissionable devices. This is implemented in the `mdns` module. The implementation may need to be changed or updated for other operating systems. The `rs-matter` crate already has multiple implementations that can be reused.

The CSA requires production devices to be certified. This example is, of course, not certified. When commissioning—i.e., pairing with standard products like Siri or Alexa—they will warn you about the missing certification. This is just a formality, and you can continue the commissioning process.

# Bridge Example

The bridge starts in a so-called "commissionable" mode if the `.matter_kvs` directory is not present in the current directory. The directory will then be created, and security keys will be stored inside it. By default, the commissioning window remains open for 15 minutes. This is set by the `MAX_COMM_WINDOW_TIMEOUT_SECS` constant from the `rs-matter` crate and can be customized. Once the bridge is commissioned successfully, it will start in commissioned mode after a restart. To clear the commission state, simply delete the `.matter_kvs` directory.

The bridge can also include custom logic. For example, a light can be configured to turn off automatically after a certain amount of time. See the code in `light_on_off.rs` to customize this logic. There is commented-out code that automatically switches the light off 1 second after it is turned on via the Matter interface.

Additionally, the bridge does not have to offer 2 lights for commissioning. Check the usage of the `SECOND_LIGHT_ENABLED` environment variable in `lib.rs` to configure whether one or two lights are presented to the commissioning party.

The bridge code uses Tokio to start multiple tasks once activated. The original code from the `rs-matter` crate uses Embassy, which is recommended for smaller devices. It is easy to switch; you just need to replace Tokio's `select!` macro in `lib.rs`.

**Known issues:**
* Once the bridge is commissioned and restarted, the first command fails with an unknown session error. The second and subsequent commands seem to work correctly.
* Some errors appear in the log indicating that certain features are not yet implemented. This is likely due to incomplete implementations that do not fully follow the Matter specification.

# Client Examples

The `client` crate uses `matc`, a separate Matter controller/client implementation. It currently builds two client binaries:

* `clientui`: an interactive terminal UI for scanning, commissioning, naming endpoints, managing commissioned devices, and invoking endpoint commands.
* `invoker`: a small command-line client for invoking one command on a saved device endpoint.

Both clients use the same local state file, `client/client-state.toml`. The terminal UI writes this file when devices are commissioned or endpoint aliases are renamed. The invoker reads it to resolve a device name and endpoint name to the saved node ID, address, and endpoint ID.

Run the terminal UI from the `client` directory:

```sh
cargo run --bin clientui
```

In the terminal UI, use `r` to scan, `c` to commission a selected commissionable device, and `m` or Enter to manage a saved commissioned device. The manage screen supports On/Off endpoints, Actions-cluster entries, endpoint alias changes, fabric label updates, and decommissioning.

Run the invoker from the `client` directory:

```sh
cargo run --bin invoker -- "<device name>" "<endpoint name>" "<action>"
```
