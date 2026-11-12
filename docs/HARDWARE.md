# Hardware

TorqOS has no hardware of its own: it runs wherever NixOS runs. It is built and tested for x86_64 only; the flake still evaluates for aarch64, but nothing builds or tests it (ADR-030, amended).

| Use | Machine |
|---|---|
| Dev | the owner's laptop (the Zenbook below, running TorqOS), shared and throttled for heavy builds |
| Full checks | "the check host": an x86_64 VM on VMware ESXi with nested virtualisation on (KVM) |
| Tests | NixOS VM tests (`tests/`), QEMU with KVM, on the check host and in CI |
| aarch64 | nothing builds or tests it; the v0.3.x releases have no aarch64 images |

The earlier dev host was itself installed from a TorqOS release image (the x86_64 VMDK, 2026-11-02), so the dev
machine is a normal install: everything done to it goes through its own broker and ledger.

## The target laptop (ADR-046)

The desktop's target machine for now, the owner's: a target to make work, not reference hardware to buy.

| | ASUS Zenbook S 16 (UM5606WA), BIOS UM5606WA.321 |
|---|---|
| CPU, GPU | AMD Ryzen AI 9 365 (Strix Point), Radeon 880M — `amdgpu`, firmware `amdgpu/` |
| Disk | Micron 2400 NVMe, 1 TB — `nvme` |
| Wi-Fi, Bluetooth | MediaTek MT7925 (PCIe `14c3:7925`; Bluetooth on USB `13d3:3608`) — `mt7925e`, `btusb`/`btmtk`, firmware `mediatek/mt7925/` |
| Network | Wi-Fi only: no Ethernet |
| Audio | ALC294 (`snd_hda_intel`) with four Cirrus CS35L56 amplifiers (firmware `cirrus/`, ASUS tuning); AMD ACP70 (SOF) |
| Input | ASUS I2C touchpad (`i2c_hid_acpi`), ELAN touchscreen and stylus (`hid_multitouch`), ASUS keys (`asus_wmi`) |
| Sensors | light and proximity through `amd_sfh` (iio-sensor-proxy) |
| Other | XDNA NPU (`amdxdna`, firmware `amdnpu/`), Thunderbolt/USB4, SD reader (`sdhci_pci`), UVC webcam |
| Known good | Pop!_OS 24.04, kernel 7.2.15 (2026-11-08); nothing out of tree |

Which of it the base carries and which the desktop: ADR-046 3. A per-model hardware profile (planned as `hardware/asus-zenbook-s16-um5606wa.nix`) is not built: there is no `hardware/` folder. The laptop was installed with the hardware file the installer detects (`nixos-generate-config`); making that file turn on redistributable firmware and iwd is an open M3.5 item.
TorqOS takes its whole NVMe disk (the owner, 2026-11-09: Pop!_OS is wiped), with the installer's erase mode (ADR-048). Pop!_OS was LUKS + LVM over the whole disk, which ADR-048 does not shrink.

### Known for this model (community reports, not yet seen on this unit)

- **Wi-Fi after suspend** (`mt7925e` not back after s2idle): fixed in 6.14; Bluetooth after resume
  reported failing on some 6.14 systems — check it.
- **Speakers** thin before 6.13 (the CS35L56 tuning); if still thin, a PipeWire upmix to 4.0 is the
  reported work-around.
- **Display freezes from panel self-refresh**: `amdgpu.dcdebugmask=0x600` is the reported fix — only if
  seen, as a kernel parameter in the profile, with `commit confirmed`.

Sources: the Arch wiki's "ASUS Zenbook UM5606"; `github.com/BNieuwenhuizen/zenbook-s16`.

### What must work (checked on the machine, dated; a failure becomes a KNOWN-GAPS line)

| Function | How to check | Status |
|---|---|---|
| Boots TorqOS, installed by erasing the disk | the installer, then the disk | installed 2026-11-09, three rounds (ROADMAP M3.5) |
| GPU accelerated (amdgpu, radeonsi, RADV) | `dmesg \| grep amdgpu`, `vulkaninfo --summary` | GPU "up", 2026-11-09 (ROADMAP M3.5); acceleration not separately recorded |
| The panel at its native mode, 120 Hz; scale | COSMIC's display settings | — |
| Brightness keys (OLED) | `/sys/class/backlight`, the keys | — |
| No freezes from panel self-refresh | a day of use | — |
| Touchscreen, pen, touchpad gestures | `libinput debug-events`, the session | — |
| Keyboard, Fn keys, keyboard light | the keys; `/sys/class/leds` | — |
| Speakers full range, microphones, headphone jack | `speaker-test -c 2`, `arecord` | speakers' ASUS tuning "up", 2026-11-09; microphones and jack not recorded |
| Webcam | `v4l2-ctl --list-devices` | — |
| Wi-Fi from setup on, no cable; after suspend | setup joins; suspend, resume, `ping` | joined in setup, 2026-11-09; after suspend not recorded |
| Bluetooth, and after suspend | `bluetoothctl` | Bluetooth "up", 2026-11-09; after suspend not recorded |
| Suspend (s2idle), the lid, drain per hour | close the lid 10 min | — |
| Battery, charge limit | `upower -d`, `charge_control_end_threshold` | — |
| Power profiles | `powerprofilesctl` | — |
| USB-C charging, a dock, an external screen, HDMI | plug in | — |
| Temperatures and fan | `sensors` | — |
| NPU seen | `/dev/accel/accel0` | — |
| A system change rolls back on this hardware | `commit confirmed`, let it expire | — |

Firmware and Secure Boot hardware guidance were dropped with the laptop goals (ADR-019); the earlier
list is in git history.
