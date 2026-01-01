# Matter toolkit


# Compiling bridge example

## Prereq

- Raspi 5 with Raspi OS

1. clone https://github.com/project-chip/connectedhomeip or fork https://github.com/skoky/connectedhomeip
2. download submodules for linux `./scripts/checkout_submodules.py --shallow --platform linux`
3. bootstrap dependencies `. scripts/bootstrap.sh`
4. build bridge `scripts/build/build_examples.py --target linux-arm64-bridge build`
5. or build light app `scripts/build/build_examples.py --target linux-arm64-lightapp build`
6. optional use `strip` to remove debug symbols
7. use a matter client to commission new device

Re-commissioning: remove kvs file and restart bridge


