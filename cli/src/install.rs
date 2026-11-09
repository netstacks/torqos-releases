//! The ISO installer (ADR-031). On the TorqOS installer's console, and nowhere else: lists the
//! disks TorqOS could go on, asks for one and for its name again, writes the TorqOS image to it —
//! the same disk CI builds and tests — reads it back, and leaves this machine's hardware, as NixOS
//! detects it, on the new disk for first-boot setup (setup.rs `hardware_file`). Nothing else runs
//! on the installer: nobody can log in, nothing listens (modules/installer.nix).

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the installer carries (modules/installer.nix): `image.raw.zst`, the image compressed;
/// `size` and `sha256`, the image's once decompressed; `file-systems.nix`, the image's disk.
const INSTALLER: &str = "/etc/torq/installer";
const BIN: &str = "/run/current-system/sw/bin";
/// Where, inside the new disk's root, first-boot setup finds the hardware detected here.
pub const DETECTED: &str = "var/lib/torq/install/hardware-configuration.nix";
/// Beside it: the registration of what the installer copied for the first build (ADR-048 5), which
/// setup loads into the new disk's Nix database before building change #1.
pub const REGISTRATION: &str = "var/lib/torq/install/first-build.registration";
/// What a first boot builds from, as the installer carries it (modules/installer.nix: closureInfo's
/// `store-paths` and `registration`).
const FIRST_BUILD: &str = "/etc/torq/installer/first-build";
const WELCOME: &str = "Welcome to the TorqOS installer. It puts TorqOS on one of this machine's disks, erasing everything on it.";
/// The label of `/var/lib/torq`'s own file system on the new disk (hosts/installer/file-systems.nix).
const TORQ_LABEL: &str = "torq";

/// The image's root file system's label (hosts/image/hardware-configuration.nix).
const ROOT_LABEL: &str = "nixos";
/// Its boot partition's (the same file).
const BOOT_LABEL: &str = "ESP";
/// What is written at a time: and so how often the progress line moves.
const CHUNK: usize = 4 << 20;
/// The end of a disk that is zeroed: what an earlier life left there (a partition table's backup,
/// a RAID or ZFS label) would otherwise outlive the install and confuse what reads the disk.
const TAIL: u64 = 1 << 20;

/// A disk TorqOS could be installed on.
#[derive(Debug, PartialEq)]
pub struct Disk {
    /// Its kernel name: `sda`, `nvme0n1`, `vda`.
    pub name: String,
    pub bytes: u64,
    /// Vendor and model, as the disk says; may be empty.
    pub model: String,
    /// How it is attached.
    pub bus: &'static str,
    /// Its partitions now, by name.
    pub parts: Vec<String>,
    /// Which disk it is, as the kernel numbers them: its `diskseq` (never reused while the machine
    /// runs: a disk unplugged and another plugged in under the same name has another) and its
    /// device number (`major:minor`). Checked again once the disk is open for writing.
    pub seq: String,
    pub dev: String,
}

/// Block devices that are never a disk to install to: loop, RAM and compressed-RAM devices,
/// optical drives, device-mapper and RAID devices, floppies, network and test block devices, and
/// an eMMC's boot and replay-protected areas (mmcblk0boot0, mmcblk0rpmb), which are not its disk.
fn not_a_disk(name: &str) -> bool {
    ["loop", "ram", "zram", "sr", "dm-", "md", "fd", "nbd", "mtdblock", "nullb"].iter().any(|p| name.starts_with(p))
        || name.ends_with("rpmb")
        || (name.starts_with("mmcblk") && name.contains("boot"))
}

/// How a disk is attached, from where it is in `/sys/devices`.
fn bus(real: &Path) -> &'static str {
    let s = real.to_string_lossy();
    // A USB disk is a USB disk, whatever bridge it has (a SATA one shows `/ata` too).
    if s.contains("/usb") {
        "USB"
    } else if s.contains("/nvme") {
        "NVMe"
    } else if s.contains("/virtio") {
        "virtio"
    } else if s.contains("/mmc") {
        "SD/eMMC"
    } else if s.contains("/ata") {
        "SATA"
    } else {
        "disk"
    }
}

/// The disks in use, from `mountinfo` (/proc/self/mountinfo) and `sys` (/sys): every disk holding
/// something mounted — the installer's own medium among them — and every disk under a device that
/// does (a loop device's file, a device-mapper or RAID device's disks).
pub fn busy(sys: &Path, mountinfo: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in mountinfo.lines() {
        // The third field is the device, major:minor; 0 is no device (proc, tmpfs, overlay …).
        match line.split(' ').nth(2) {
            Some(dev) if !dev.starts_with("0:") => mark(&sys.join("dev/block").join(dev), &mut out, 0),
            _ => {}
        }
    }
    out
}

/// `dev` (a block device's directory in /sys) and what it is made of, as their disks' names.
fn mark(dev: &Path, out: &mut BTreeSet<String>, depth: usize) {
    let Ok(real) = std::fs::canonicalize(dev) else { return };
    // A partition's disk is the directory it is in.
    let disk = if real.join("partition").exists() { real.parent() } else { Some(real.as_path()) };
    if let Some(name) = disk.and_then(Path::file_name) {
        out.insert(name.to_string_lossy().into_owned());
    }
    if depth < 8 {
        if let Ok(slaves) = std::fs::read_dir(real.join("slaves")) {
            for s in slaves.flatten() {
                mark(&s.path(), out, depth + 1);
            }
        }
    }
}

/// Whether anything is built on `dir` (a disk's or a partition's directory in /sys): a
/// device-mapper or RAID device it is part of.
fn held(dir: &Path) -> bool {
    std::fs::read_dir(dir.join("holders")).is_ok_and(|mut r| r.next().is_some())
}

/// The disks TorqOS could be installed on, from `sys` (/sys): whole, writable disks with a size,
/// none in use (`busy`) or part of anything (an LVM volume group, a RAID array), none of the
/// paths an NVMe disk with several controllers hides behind its own (`hidden`).
pub fn disks(sys: &Path, busy: &BTreeSet<String>) -> Result<Vec<Disk>> {
    let block = sys.join("block");
    let mut names: Vec<String> = std::fs::read_dir(&block).with_context(|| format!("reading {}", block.display()))?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let d = block.join(&name);
        let read = |f: &str| std::fs::read_to_string(d.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
        if not_a_disk(&name) || busy.contains(&name) || read("ro") == "1" || read("hidden") == "1" {
            continue;
        }
        // The image's partition table is in 512-byte sectors: on a disk with larger ones (a 4Kn
        // drive, some USB bridges) the kernel would find no partitions in it.
        if !matches!(read("queue/logical_block_size").as_str(), "" | "512") {
            continue;
        }
        let parts: Vec<PathBuf> = std::fs::read_dir(&d).map(|r| r.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|f| f.to_string_lossy().starts_with(&name)) && p.join("partition").exists()).collect()).unwrap_or_default();
        if held(&d) || parts.iter().any(|p| held(p)) {
            continue;
        }
        // In 512-byte sectors, whatever the disk's own.
        let bytes = read("size").parse::<u64>().unwrap_or(0) * 512;
        if bytes == 0 {
            continue;
        }
        // "ATA" is a SATA disk's vendor as Linux reads it: not the maker.
        let vendor = read("device/vendor");
        let model = [if vendor == "ATA" { String::new() } else { vendor }, read("device/model")].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
        let bus = bus(&std::fs::canonicalize(&d).unwrap_or(d.clone()));
        let (seq, dev) = (read("diskseq"), read("dev"));
        let mut parts: Vec<String> = parts.iter().filter_map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned())).collect();
        parts.sort();
        out.push(Disk { name, bytes, model, bus, parts, seq, dev });
    }
    Ok(out)
}

/// A size as disks are sold: decimal, one decimal place below ten.
pub fn size(bytes: u64) -> String {
    let units = ["B", "kB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 || v >= 10.0 { format!("{v:.0} {}", units[u]) } else { format!("{v:.1} {}", units[u]) }
}

/// One disk's line in the list.
fn describe(d: &Disk, needed: u64) -> String {
    let model = if d.model.is_empty() { "(no model name)" } else { &d.model };
    let now = match d.parts.len() {
        0 => "no partitions".to_string(),
        1 => "1 partition".to_string(),
        n => format!("{n} partitions"),
    };
    let small = if d.bytes < needed { format!(" — too small: TorqOS needs {}", size(needed)) } else { String::new() };
    format!("{:<10} {:>8}  {:<7} {model}, {now}{small}", d.name, size(d.bytes), d.bus)
}

/// What the installer finds on this machine that NixOS's detection does not say (ADR-046).
/// Firmware it does: on real hardware nixos-generate-config imports `not-detected.nix`, which turns on
/// redistributable firmware, and the CPU's microcode with it.
#[derive(Debug, Default, PartialEq)]
pub struct Found {
    /// A Wi-Fi device: a wireless network interface, or a PCI device of the wireless class (0x0280),
    /// seen even before its driver has its firmware.
    pub wireless: bool,
}

/// What `Found` says, looked for under `sys` (`/sys`, or a test's folder).
pub fn found(sys: &Path) -> Found {
    let any = |dir: &str, is: &dyn Fn(&Path) -> bool| {
        std::fs::read_dir(sys.join(dir)).map(|d| d.flatten().any(|e| is(&e.path()))).unwrap_or(false)
    };
    let interface = any("class/net", &|p| p.join("wireless").exists() || p.join("phy80211").exists());
    let pci = any("bus/pci/devices", &|p| std::fs::read_to_string(p.join("class")).is_ok_and(|c| c.trim().starts_with("0x0280")));
    Found { wireless: interface || pci }
}

/// `found` as a NixOS module, or nothing when there is nothing to say.
fn found_module(f: &Found) -> Option<String> {
    f.wireless.then(|| {
        "\x20   # A Wi-Fi device was found here (ADR-046): iwd joins networks (`iwctl`), networkd configures\n\
         \x20   # them; the networks and their passphrases are iwd's state, never the repository's.\n\
         \x20   ({ ... }: { networking.wireless.iwd.enable = true; })\n"
            .to_string()
    })
}

/// This machine's hardware file: what NixOS detected here (`detected`: nixos-generate-config
/// `--show-hardware-config --no-filesystems`), what the installer found (`found`) and the image's disk
/// (`file_systems`), each a NixOS module as it was written, one file.
pub fn hardware_file(detected: &str, found: &Found, file_systems: &str) -> Result<String> {
    if detected.trim().is_empty() {
        bail!("NixOS detected no hardware here (nixos-generate-config said nothing)");
    }
    // The file systems are the image's: anything else would describe disks that are not there.
    if detected.contains("fileSystems") || detected.contains("swapDevices") {
        bail!("the detected hardware names file systems; it should not (--no-filesystems)");
    }
    Ok(format!(
        "# This machine's hardware (ADR-031): what the TorqOS installer detected on it, and the disk of the\n\
         # TorqOS image it was installed from. Attached at the top of the tree (config-files.json).\n\
         {{ ... }}:\n\
         {{\n\
         \x20 imports = [\n\
         \x20   # Detected by the installer (nixos-generate-config --show-hardware-config --no-filesystems):\n\
         \x20   (\n\
         {}\n\
         \x20   )\n\
         {}\
         \x20   # The TorqOS image's disk:\n\
         \x20   (\n\
         {}\n\
         \x20   )\n\
         \x20 ];\n\
         }}\n",
        detected.trim_end(),
        found_module(found).unwrap_or_default(),
        file_systems.trim_end()
    ))
}

/// A disk's partition `n`'s name: `sda` 2 is `sda2`, `nvme0n1` 2 is `nvme0n1p2`.
pub fn partition(disk: &str, n: u32) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) { format!("{disk}p{n}") } else { format!("{disk}{n}") }
}

fn hex(h: Sha256) -> String {
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// One of the installer's tools, which must succeed; its standard output.
fn tool(name: &str, args: &[&str]) -> Result<String> {
    let o = Command::new(format!("{BIN}/{name}")).args(args).stdin(Stdio::null()).output().with_context(|| format!("running {name}"))?;
    if !o.status.success() {
        bail!("{name} {} failed: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn read_meta(name: &str) -> Result<String> {
    let p = Path::new(INSTALLER).join(name);
    Ok(std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?.trim().to_string())
}

fn welcome() -> String {
    match std::fs::read_to_string(crate::setup::BANNER) {
        Ok(banner) => format!("{}\n\n{WELCOME}", banner.trim_end()),
        Err(_) => WELCOME.to_string(),
    }
}

/// A file system's label on `dev`, read from the device itself (`-p`: not blkid's cache).
fn label(dev: &str) -> Option<String> {
    tool("blkid", &["-p", "-o", "value", "-s", "LABEL", "--", dev]).ok().map(|l| l.trim().to_string())
}

/// Whether `d` carries the installer itself: the disk or one of its partitions has the ISO's volume
/// label (`volume`). Mounted, the installer's medium is `busy` already; copied to RAM (its boot
/// menu's "copy to RAM" entry), it is not mounted, and only this says whose it is.
fn carries_installer(d: &Disk, volume: &str) -> bool {
    !volume.is_empty() && std::iter::once(&d.name).chain(&d.parts).any(|p| label(&format!("/dev/{p}")).as_deref() == Some(volume))
}

const DAMAGED: &str = "The installer's copy of TorqOS is damaged (its checksum differs): make the installer again from a checked download.";

/// The image, decompressed, a piece at a time to `each` (with how much has come so far): it must be
/// `bytes` long with the SHA-256 `sha`, or the installer's copy is damaged.
fn stream(image: &Path, bytes: u64, sha: &str, mut each: impl FnMut(&[u8], u64) -> Result<()>) -> Result<()> {
    // The file itself, not /etc's link to it: zstd skips a link given it (and says so only as a
    // warning). Not quiet, so what it says is all in its complaint.
    let image = std::fs::canonicalize(image).with_context(|| format!("finding {}", image.display()))?;
    let mut z = Command::new(format!("{BIN}/zstd")).args(["-dc", "--"]).arg(&image).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("running zstd")?;
    let mut src = z.stdout.take().context("zstd's output")?;
    // zstd's complaint, if any: a line or two, read once it has exited (too little to fill the pipe).
    let mut why = z.stderr.take().context("zstd's errors")?;
    let read = pieces(&mut src, bytes, &mut each);
    if read.is_err() {
        let _ = z.kill();
    }
    let status = z.wait().context("zstd")?;
    let (n, h) = read?;
    if !status.success() {
        let mut said = String::new();
        let _ = why.read_to_string(&mut said);
        bail!("{DAMAGED}\n  (zstd: {status}: {})", said.trim());
    }
    whole(n, &h, bytes, sha)
}

/// `src` a piece at a time to `each` (with how much has come so far), no more than `bytes` of it:
/// how much came, and its SHA-256.
fn pieces(src: &mut impl Read, bytes: u64, mut each: impl FnMut(&[u8], u64) -> Result<()>) -> Result<(u64, String)> {
    let (mut buf, mut h, mut n) = (vec![0u8; CHUNK], Sha256::new(), 0u64);
    loop {
        let k = src.read(&mut buf).context("reading the image")?;
        if k == 0 {
            return Ok((n, hex(h)));
        }
        n += k as u64;
        if n > bytes {
            bail!(DAMAGED);
        }
        h.update(&buf[..k]);
        each(&buf[..k], n)?;
    }
}

/// The installer's copy is the image only if all `bytes` of it came (`n`), with its SHA-256 (`h`).
fn whole(n: u64, h: &str, bytes: u64, sha: &str) -> Result<()> {
    if n != bytes || h != sha {
        bail!(DAMAGED);
    }
    Ok(())
}

/// A progress line, redrawn at most twice a second (a console redraws slowly).
struct Shown(Option<std::time::Instant>);

impl Shown {
    fn show(&mut self, what: &str, n: u64, of: u64) {
        if n == of || self.0.is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(500)) {
            print!("\r  {what}: {} of {}  ", size(n), size(of));
            let _ = std::io::stdout().flush();
            self.0 = Some(std::time::Instant::now());
        }
    }
}

/// The installer's console: check its own copy of TorqOS, then choose, write, detect; again from
/// the start if anything fails.
pub fn run() -> Result<()> {
    if !rustix::process::geteuid().is_root() {
        bail!("install is the TorqOS installer's, run as root on its console");
    }
    let image = Path::new(INSTALLER).join("image.raw.zst");
    if !image.exists() {
        bail!("install is the TorqOS installer's: this machine is not running it");
    }
    let bytes: u64 = read_meta("size")?.parse().context("the image's size")?;
    let sha = read_meta("sha256")?;
    // No key on the console stops the installer mid-write (Ctrl-C, Ctrl-\, Ctrl-Z would kill or
    // stop it with half an image on the disk): a question is still answered, or left, by its line.
    // A console whose keys cannot be set so is not one to write a disk from: nothing is offered.
    let stdin = std::io::stdin();
    let mut t = rustix::termios::tcgetattr(&stdin).context("reading the console's settings (so that no key stops a write)")?;
    t.local_modes.remove(rustix::termios::LocalModes::ISIG);
    rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &t).context("setting the console so that no key stops a write")?;
    // Before any disk is offered: a damaged stick or download is found here, with nothing written.
    println!("\n{}\n\n  Checking the installer's own copy of TorqOS ...", welcome());
    let mut shown = Shown(None);
    if let Err(e) = stream(&image, bytes, &sha, |_, n| {
        shown.show("Checked", n, bytes);
        Ok(())
    }) {
        println!("\n\n{e:#}");
        crate::setup::line_under(&welcome, "Press Enter to turn the machine off", "")?;
        let _ = Command::new(format!("{BIN}/systemctl")).arg("poweroff").status();
        return Ok(());
    }
    // The welcome is on the screen already, above the check: the disks follow it.
    println!("\n");
    let mut closed = 0;
    loop {
        let done = install(&image, bytes, &sha);
        closed = if done.as_ref().is_err_and(|e| format!("{e:#}").contains("the console closed")) { closed + 1 } else { 0 };
        match done {
            Ok(name) => {
                println!("\nTorqOS is on {name}. Remove the installer (its USB stick or disc) and restart: the machine starts TorqOS's setup.");
                crate::setup::line_under(&welcome, "Press Enter to restart", "")?;
                let _ = Command::new(format!("{BIN}/systemctl")).arg("reboot").status();
                return Ok(());
            }
            // A Ctrl-D starts over; a console that only ever reads as closed has nobody to start
            // over for: ended, for systemd to start the installer again.
            Err(e) if closed >= 3 => return Err(e),
            Err(e) => println!("\n{e:#}\nStarting over.\n\n{}\n", welcome()),
        }
    }
}

/// Choose a disk, write the image (`bytes` long, SHA-256 `sha`) to it, leave the detected hardware
/// on it; the disk's name.
fn install(image: &Path, bytes: u64, sha: &str) -> Result<String> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").context("reading what is mounted")?;
    let sys = Path::new("/sys");
    let volume = read_meta("volume").unwrap_or_default();
    let disks: Vec<Disk> = disks(sys, &busy(sys, &mountinfo))?.into_iter().filter(|d| !carries_installer(d, &volume)).collect();
    if disks.is_empty() {
        crate::setup::line_under(&welcome, "There is no disk to install to (the installer's own is not offered). Attach one, then press Enter", "")?;
        bail!("No disk was chosen.");
    }
    let list = disks.iter().enumerate().map(|(i, d)| format!("  {}  {}", i + 1, describe(d, bytes))).collect::<Vec<_>>().join("\n");
    println!("Disks:\n{list}\n");
    // Shown again with the question, if the console is taken over (setup.rs `asked`).
    let with_list = || format!("{}\n\nDisks:\n{list}", welcome());
    let default = if disks.len() == 1 { "1" } else { "" };
    let d = crate::setup::until(
        || crate::setup::line_under(&with_list, &format!("Install TorqOS on (1-{})", disks.len()), default),
        |c| {
            let d = c.parse::<usize>().ok().filter(|n| (1..=disks.len()).contains(n)).map(|n| &disks[n - 1]);
            match d {
                None => bail!("a number from 1 to {}", disks.len()),
                Some(d) if d.bytes < bytes => bail!("{} is too small: TorqOS needs {}", d.name, size(bytes)),
                Some(d) => Ok(d),
            }
        },
    )?;
    let model = if d.model.is_empty() { String::new() } else { format!(", {}", d.model) };
    let warning = format!("Everything on {} ({}{model}) will be erased.", d.name, size(d.bytes));
    println!("\n{warning}");
    let with_warning = || format!("{}\n\n{warning}", welcome());
    if crate::setup::line_under(&with_warning, &format!("Type its name, {}, to go ahead", d.name), "")? != d.name {
        bail!("Not confirmed: nothing was written.");
    }
    others_labelled(d)?;
    write(image, bytes, sha, d)?;
    if let Err(e) = detect(d) {
        // Without the detected hardware, setup would take the image's (a VM's): no half-installed
        // system left to start from.
        // The disk the name still means, checked as it was before writing (`by_name`).
        let now = match by_name(d, true).and_then(|mut f| clear(&mut f, bytes, d.bytes)) {
            Ok(()) => format!("{} was cleared again: it holds no system.", d.name),
            Err(_) => format!("{} may hold part of TorqOS: install again before starting from it.", d.name),
        };
        bail!("{e:#}\n{now}");
    }
    Ok(d.name.clone())
}

/// The installed system finds its file systems by their labels (`file-systems.nix`): another disk
/// with a file system labelled the same — an earlier NixOS's root, another ESP — could be the one
/// it starts from, or grows, or puts its boot loader on. Refused until that disk is out or cleared.
fn others_labelled(d: &Disk) -> Result<()> {
    for want in [ROOT_LABEL, BOOT_LABEL, TORQ_LABEL] {
        // Probed now, not from blkid's cache.
        let o = Command::new(format!("{BIN}/blkid")).args(["-c", "/dev/null", "-o", "device", "-t", &format!("LABEL={want}")]).stdin(Stdio::null()).output().context("running blkid")?;
        let found = labelled(o.status.code(), &String::from_utf8_lossy(&o.stdout), &String::from_utf8_lossy(&o.stderr), want)?;
        not_elsewhere(d, &found, want)?;
    }
    Ok(())
}

/// What blkid found labelled `want`, from how it ended (`code`) and what it said: none only when
/// it says so (exit 2, nothing listed). Any other failure is not knowing, and nothing is written.
fn labelled(code: Option<i32>, out: &str, err: &str, want: &str) -> Result<String> {
    match code {
        Some(0) => Ok(out.to_string()),
        Some(2) if out.trim().is_empty() => Ok(String::new()),
        _ => bail!("Could not look for other disks with a file system labelled {want} (blkid: {}): nothing was written.", err.trim()),
    }
}

/// Refused if a device in `found` (blkid's list, one per line) is not `d` or one of its partitions.
fn not_elsewhere(d: &Disk, found: &str, want: &str) -> Result<()> {
    for dev in found.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let name = Path::new(dev).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        if name != d.name && !d.parts.contains(&name) {
            bail!(
                "{dev} (not on {}) has a file system labelled {want}, as TorqOS's own is: the installed TorqOS could start from it instead. Disconnect that disk, or clear it, then install again. Nothing was written.",
                d.name
            );
        }
    }
    Ok(())
}

/// Clear what makes `f`, a disk `disk` bytes long that may hold the image's first `bytes` or part
/// of them, look like a system: its partition tables — at its start (the first MiB, before any
/// partition), the image's backup at the image's end, and the disk's end (an earlier backup some
/// firmware would restore from). What the partitions held is left, with nothing to find it by.
fn clear(f: &mut std::fs::File, bytes: u64, disk: u64) -> Result<()> {
    let zero = vec![0u8; TAIL as usize];
    for at in [0, bytes.saturating_sub(TAIL), disk.saturating_sub(TAIL)] {
        f.seek(SeekFrom::Start(at))?;
        f.write_all(&zero)?;
    }
    f.sync_all()?;
    Ok(())
}

/// `d`, open for writing, and nothing else: exclusive (the kernel refuses a disk something holds —
/// mounted, a RAID's, a swap's), and checked to be the disk that was listed and chosen.
fn open(d: &Disk) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let dev = Path::new("/dev").join(&d.name);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(rustix::fs::OFlags::EXCL.bits() as i32)
        .open(&dev)
        .with_context(|| format!("{} is in use: nothing was written", d.name))?;
    is_disk(d, &f, Path::new("/sys")).context("nothing was written")?;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").context("reading what is mounted")?;
    if busy(Path::new("/sys"), &mountinfo).contains(&d.name) {
        bail!("{} is in use now: nothing was written.", d.name);
    }
    Ok(f)
}

/// `d` opened again by its name, after the write (to read it back, find its partitions, clear it):
/// checked to be the disk written, as `open` checked it before (`is_disk`).
fn by_name(d: &Disk, write: bool) -> Result<std::fs::File> {
    let f = std::fs::OpenOptions::new().read(true).write(write).open(Path::new("/dev").join(&d.name)).with_context(|| format!("opening {} again", d.name))?;
    is_disk(d, &f, Path::new("/sys"))?;
    Ok(f)
}

/// The device number `f` is (it is open on a block device): `major:minor`.
fn devno(f: &std::fs::File) -> Result<String> {
    let rdev = rustix::fs::fstat(f).context("which device it is")?.st_rdev as u64;
    Ok(format!("{}:{}", rustix::fs::major(rdev), rustix::fs::minor(rdev)))
}

/// A path any process, the installer's tools among them, opens as `f` itself: not by a name that
/// could mean another disk by then.
fn fd_path(f: &std::fs::File) -> String {
    use std::os::fd::AsRawFd;
    format!("/proc/{}/fd/{}", std::process::id(), f.as_raw_fd())
}

/// Whether `f`, opened by `d`'s name, is the disk `d` that was listed: unplugged and another
/// plugged in, a name can mean another disk. `sys` is /sys.
fn is_disk(d: &Disk, f: &std::fs::File, sys: &Path) -> Result<()> {
    same_disk(d, &devno(f)?, sys)
}

/// Whether the block device `now` (`major:minor`) is `d`: its device number, `diskseq` and size.
fn same_disk(d: &Disk, now: &str, sys: &Path) -> Result<()> {
    let dir = sys.join("dev/block").join(now);
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
    let now_bytes = read("size").parse::<u64>().unwrap_or(0) * 512;
    if now != d.dev || read("diskseq") != d.seq || now_bytes != d.bytes {
        bail!("{} is not the disk that was chosen any more (was it replaced?).", d.name);
    }
    Ok(())
}

/// Whether the block device `now` (`major:minor`) is a partition of `d`.
fn part_of(d: &Disk, now: &str, sys: &Path) -> Result<()> {
    let dir = std::fs::canonicalize(sys.join("dev/block").join(now)).with_context(|| format!("finding device {now}"))?;
    let disk = dir.parent().filter(|_| dir.join("partition").exists());
    let read = |f: &str| disk.and_then(|p| std::fs::read_to_string(p.join(f)).ok()).map(|s| s.trim().to_string()).unwrap_or_default();
    if read("dev") != d.dev || read("diskseq") != d.seq {
        bail!("device {now} is not a partition of {}, the disk that was written (was it replaced?).", d.name);
    }
    Ok(())
}

/// The first MiB of the image: its protective MBR and partition table, and nothing of any
/// partition's (the boot partition starts at 8 MiB: NixOS's image builder, `efi`).
const HEAD: u64 = 1 << 20;

/// What a source of the image (`stream`) is given: each piece, with how much has come so far.
type Each<'a> = &'a mut dyn FnMut(&[u8], u64) -> Result<()>;

/// Write the image (`bytes` long, SHA-256 `sha`) to `d` and read it back. Once anything is written,
/// a failure clears the disk's start again; and the partition table is written last, so that until
/// every other byte is on the disk — whatever stops the write: a reset, a power cut — there is no
/// partition table to start a half-written system from.
fn write(image: &Path, bytes: u64, sha: &str, d: &Disk) -> Result<()> {
    let mut out = open(d)?;
    let name = d.name.as_str();
    let mut shown = Shown(None);
    let wrote = put(&mut out, bytes, d.bytes, |each| stream(image, bytes, sha, each), |n| shown.show(&format!("Writing TorqOS to {name}"), n, bytes))
        .with_context(|| format!("writing to {name}"));
    println!();
    let checked = wrote.and_then(|()| read_back(d, bytes, sha));
    if let Err(e) = checked {
        let cleared = clear(&mut out, bytes, d.bytes);
        let now = if cleared.is_ok() { format!("{name} was cleared again: it holds no system.") } else { format!("{name} may hold part of TorqOS: install again before starting from it.") };
        bail!("{e:#}\n{now}");
    }
    Ok(())
}

/// The image to `out`, a disk `disk` bytes long, from `src` (which passes each piece of the
/// image's `bytes` to the function it is given, and fails if they are not the image): the disk's
/// first `HEAD` and, past the image, its last `TAIL` zeroed first — an earlier partition table and
/// its backup at the disk's end — and the image's own first `HEAD` written last, after everything
/// else is on the disk. `progress` is how much has come.
fn put(
    out: &mut std::fs::File,
    bytes: u64,
    disk: u64,
    src: impl FnOnce(Each) -> Result<()>,
    mut progress: impl FnMut(u64),
) -> Result<()> {
    // An earlier partition table goes first, its backup at the disk's end with it: what follows is
    // not a disk anything starts from, whichever end a firmware reads.
    let head_len = HEAD.min(bytes) as usize;
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&vec![0u8; head_len])?;
    // The disk's last TAIL, whether past the image or its own end (a disk exactly the image's size):
    // what of it is the image's, the image writes again as it comes.
    let at = disk.saturating_sub(TAIL).max(head_len as u64);
    if disk > at {
        out.seek(SeekFrom::Start(at))?;
        out.write_all(&vec![0u8; (disk - at) as usize]).context("clearing the disk's end")?;
    }
    out.sync_all()?;
    let mut head = Vec::with_capacity(head_len);
    out.seek(SeekFrom::Start(head_len as u64))?;
    src(&mut |piece, n| {
        let at = n - piece.len() as u64;
        let held = (head_len as u64).saturating_sub(at).min(piece.len() as u64) as usize;
        head.extend_from_slice(&piece[..held]);
        out.write_all(&piece[held..])?;
        progress(n);
        Ok(())
    })?;
    out.sync_all()?;
    if head.len() != head_len {
        bail!("the image's start did not come whole");
    }
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&head)?;
    out.sync_all().context("finishing the writes")
}

/// Read the first `bytes` of `d` back: they must have the SHA-256 `sha`.
fn read_back(d: &Disk, bytes: u64, sha: &str) -> Result<()> {
    let name = d.name.as_str();
    let f = by_name(d, false)?;
    // What the disk holds, not the kernel's copy of what was written.
    tool("blockdev", &["--flushbufs", &fd_path(&f)])?;
    let mut shown = Shown(None);
    let ok = kept(&mut (&f).take(bytes), bytes, sha, |n| shown.show(&format!("Checking {name}"), n, bytes)).with_context(|| format!("reading {name} back"))?;
    println!();
    if !ok {
        bail!("{name} did not keep what was written to it (it reads back differently): the disk may be failing. Choose another, or check this one.");
    }
    Ok(())
}

/// Whether `src` holds `bytes` with the SHA-256 `sha`: no more, no less, nothing else.
fn kept(src: &mut impl Read, bytes: u64, sha: &str, mut progress: impl FnMut(u64)) -> Result<bool> {
    let mut buf = vec![0u8; CHUNK];
    let (mut h, mut n) = (Sha256::new(), 0u64);
    loop {
        let k = src.read(&mut buf)?;
        if k == 0 {
            break;
        }
        n += k as u64;
        h.update(&buf[..k]);
        progress(n);
    }
    Ok(n == bytes && hex(h) == sha)
}

/// Detect this machine's hardware and leave it in the new disk's root, for setup.
fn detect(d: &Disk) -> Result<()> {
    println!("  Detecting this machine's hardware ...");
    // The kernel reads the new partition table: once udev, which reads it too when a written disk
    // is closed, is done with it (until then the disk is busy).
    // A device elsewhere that never settles only delays this (as it does the installer's start,
    // modules/installer.nix): what counts is that the partitions are read.
    let settle = || {
        let _ = tool("udevadm", &["settle", "--timeout=60"]);
    };
    settle();
    let mut tries = 0;
    // Each time the disk that was written (`by_name`), and blockdev given it, not its name.
    while let Err(e) = by_name(d, false).and_then(|f| tool("blockdev", &["--rereadpt", &fd_path(&f)])) {
        tries += 1;
        if tries == 10 {
            return Err(e.context(format!("reading the partitions TorqOS put on {}", d.name)));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        settle();
    }
    settle();
    // The record's file system, by its label, among this disk's partitions only: each checked to be one
    // of the disk written, and from then on used as what was opened, not by its name.
    // A file system by its label, among this disk's partitions only: each checked to be one of the disk
    // written, and from then on used as what was opened, not by its name.
    let own = |want: &str, what: &str| {
        (1..=16)
            .map(|n| format!("/dev/{}", partition(&d.name, n)))
            .filter(|p| Path::new(p).exists())
            .filter_map(|p| std::fs::File::open(&p).ok())
            .find(|f| devno(f).and_then(|now| part_of(d, &now, Path::new("/sys"))).is_ok() && label(&fd_path(f)).as_deref() == Some(want))
            .with_context(|| format!("TorqOS's {what} is not on {} after writing it", d.name))
    };
    let record = own(TORQ_LABEL, "record file system")?;
    let detected = tool("nixos-generate-config", &["--show-hardware-config", "--no-filesystems"])?;
    let file = hardware_file(&detected, &found(Path::new("/sys")), &read_meta("file-systems.nix")?)?;
    // What a first boot builds from (ADR-048 5), where this installer carries it; none on a platform
    // it is not computed for.
    let first = Path::new(FIRST_BUILD);
    let carried = first.join("store-paths").is_file().then(|| std::fs::read_to_string(first.join("store-paths"))).transpose().context("reading what the installer carries")?;
    on(&record, "ext4", None, d, "record file system", |at| {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let put = |rel: &str, bytes: &[u8]| -> Result<()> {
            let p = at.join(rel.strip_prefix("var/lib/torq/").context("where it goes")?);
            std::fs::DirBuilder::new().recursive(true).mode(0o755).create(p.parent().context("its folder")?)?;
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o644).open(&p)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            Ok(())
        };
        put(DETECTED, file.as_bytes()).context("writing the detected hardware to the new disk")?;
        if carried.is_some() {
            let reg = std::fs::read(first.join("registration")).context("reading the carried paths' registration")?;
            put(REGISTRATION, &reg).context("writing the carried paths' registration to the new disk")?;
        }
        Ok(())
    })?;
    if let Some(paths) = carried {
        let root = own(ROOT_LABEL, "root file system")?;
        on(&root, "btrfs", Some(c"subvol=@"), d, "root file system", |at| {
            println!("  Copying what this machine's first build needs ...");
            let n = carry(&paths, Path::new("/"), &at.join("nix/store"))?;
            println!("  {n} added to the new disk's store.");
            Ok(())
        })?;
    }
    Ok(())
}

/// `run` with `fs` (a partition of `d`, opened) mounted at a folder of its own, as `fstype` with `data`;
/// unmounted after, whatever happened.
fn on(fs: &std::fs::File, fstype: &str, data: Option<&std::ffi::CStr>, d: &Disk, what: &str, run: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let mnt = tempfile::tempdir().context("a place to mount the new disk")?;
    let at = mnt.path().to_path_buf();
    // The kernel mounts the partition opened above, through its open file.
    rustix::mount::mount(fd_path(fs).as_str(), &at, fstype, rustix::mount::MountFlags::empty(), data)
        .with_context(|| format!("mounting TorqOS's {what} from {} at {}", d.name, at.display()))?;
    let done = run(&at);
    // Unmounted whatever happened: lazily if it is still busy, so the disk is not left held.
    if rustix::mount::unmount(&at, rustix::mount::UnmountFlags::empty()).is_err() {
        if let Err(e) = rustix::mount::unmount(&at, rustix::mount::UnmountFlags::DETACH) {
            // Still mounted: the folder is kept, not emptied (which would empty the new disk).
            let _ = mnt.keep();
            return Err(anyhow::Error::from(e).context(format!("unmounting TorqOS's {what} from {}", at.display())));
        }
    }
    done
}

/// Copy into `store` (the new disk's /nix/store) each of `paths` (one store path a line, as
/// closureInfo's `store-paths`) that it does not have, from under `from` (`/`: the installer's own
/// store). Each is copied under a name of its own first and renamed whole, so a path half copied
/// is never there by its name. How many were added.
pub fn carry(paths: &str, from: &Path, store: &Path) -> Result<usize> {
    let mut added = 0;
    for p in paths.lines().map(str::trim).filter(|p| !p.is_empty()) {
        let name = p.strip_prefix("/nix/store/").filter(|n| !n.is_empty() && !n.contains('/') && !n.starts_with('.')).with_context(|| format!("{p:?} is not a store path"))?;
        let to = store.join(name);
        if to.symlink_metadata().is_ok() {
            continue;
        }
        let part = store.join(format!(".{name}.torq-copy"));
        if part.symlink_metadata().is_ok() {
            tool("rm", &["-rf", &part.to_string_lossy()])?;
        }
        let src = from.join(p.trim_start_matches('/'));
        tool("cp", &["-a", "--no-target-directory", &src.to_string_lossy(), &part.to_string_lossy()]).with_context(|| format!("copying {name} to the new disk"))?;
        std::fs::rename(&part, &to).with_context(|| format!("putting {name} in the new disk's store"))?;
        added += 1;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A /sys with one disk: `devices/<at>/block/<name>`, `block/<name>` a link to it, its
    /// partitions, and `dev/block/<major>:<minor>` links.
    fn disk(sys: &Path, at: &str, name: &str, sectors: u64, model: &str, parts: &[(&str, &str)], dev: &str) {
        let d = sys.join("devices").join(at).join("block").join(name);
        std::fs::create_dir_all(d.join("device")).unwrap();
        std::fs::write(d.join("size"), format!("{sectors}\n")).unwrap();
        std::fs::write(d.join("ro"), "0\n").unwrap();
        std::fs::write(d.join("dev"), format!("{dev}\n")).unwrap();
        std::fs::write(d.join("diskseq"), format!("{}\n", dev.len())).unwrap();
        std::fs::write(d.join("device/model"), format!("{model}\n")).unwrap();
        std::fs::create_dir_all(sys.join("block")).unwrap();
        symlink(&d, sys.join("block").join(name)).unwrap();
        std::fs::create_dir_all(sys.join("dev/block")).unwrap();
        symlink(&d, sys.join("dev/block").join(dev)).unwrap();
        for (p, pdev) in parts {
            std::fs::create_dir_all(d.join(p)).unwrap();
            std::fs::write(d.join(p).join("partition"), "1\n").unwrap();
            symlink(d.join(p), sys.join("dev/block").join(pdev)).unwrap();
        }
    }

    /// A disk with sectors larger than 512 bytes (where the image's partition table is not one) is
    /// not offered; one that says nothing of its sectors is.
    #[test]
    fn a_disk_with_large_sectors_is_not_offered() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:01:00.0/nvme/nvme0/nvme0n1", "nvme0n1", 244_190_646, "4Kn", &[], "259:0");
        disk(sys, "pci0000:00/0000:02:00.0/nvme/nvme1/nvme1n1", "nvme1n1", 1_000_215_216, "512e", &[], "259:1");
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 1_000_215_216, "old", &[], "8:0");
        for (name, sector) in [("nvme0n1", "4096"), ("nvme1n1", "512")] {
            let q = std::fs::canonicalize(sys.join("block").join(name)).unwrap().join("queue");
            std::fs::create_dir_all(&q).unwrap();
            std::fs::write(q.join("logical_block_size"), format!("{sector}\n")).unwrap();
        }
        let d = disks(sys, &BTreeSet::new()).unwrap();
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["nvme1n1", "sda"]);
    }

    /// Clearing a disk after a failed install: its start, where the image's backup partition table
    /// is, and its own end are zeros; the rest is left.
    #[test]
    fn clearing_leaves_no_partition_table() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("disk");
        let (image, disk) = (5 * TAIL + 7, 9 * TAIL);
        std::fs::write(&p, vec![0xa5u8; disk as usize]).unwrap();
        let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        clear(&mut f, image, disk).unwrap();
        let got = std::fs::read(&p).unwrap();
        let zero = |a: u64, b: u64| got[a as usize..b as usize].iter().all(|&x| x == 0);
        assert!(zero(0, TAIL) && zero(image - TAIL, image) && zero(disk - TAIL, disk));
        assert!(got[TAIL as usize..(image - TAIL) as usize].iter().all(|&x| x == 0xa5), "the rest is left as it was");
        assert_eq!(got.len() as u64, disk, "nothing past the disk's end");
    }

    /// An image of `n` bytes, not all alike, and its SHA-256.
    fn image(n: usize) -> (Vec<u8>, String) {
        let v: Vec<u8> = (0..n).map(|i| (i * 7 + i / 4093) as u8).collect();
        let mut h = Sha256::new();
        h.update(&v);
        (v, hex(h))
    }

    /// A source as `put` takes it: `img` in pieces of `step`, then `end` (the stream's verdict).
    fn source(img: &[u8], step: usize, upto: usize, end: Result<()>) -> impl FnOnce(Each) -> Result<()> + '_ {
        move |each| {
            let mut n = 0;
            for piece in img[..upto].chunks(step) {
                n += piece.len() as u64;
                each(piece, n)?;
            }
            end
        }
    }

    /// Written whole: the disk holds the image, and past it, its last TAIL is zeros.
    #[test]
    fn a_whole_write_is_the_image() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("disk");
        let (img, _) = image(3 * HEAD as usize + 12_345);
        let disk = 6 * TAIL;
        std::fs::write(&p, vec![0xa5u8; disk as usize]).unwrap();
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
        // Pieces that straddle the end of the held-back head.
        put(&mut f, img.len() as u64, disk, source(&img, 300_007, img.len(), Ok(())), |_| {}).unwrap();
        let got = std::fs::read(&p).unwrap();
        assert!(got[..img.len()] == img[..], "the image, byte for byte");
        assert!(got[(disk - TAIL) as usize..].iter().all(|&x| x == 0), "the disk's end cleared");
        assert!(got[img.len()..(disk - TAIL) as usize].iter().all(|&x| x == 0xa5), "between, left");
    }

    /// Stopped at any point before the end — the image found damaged, the source cut off, the
    /// process gone — the disk has no partition table: its first MiB, where the image's is, is
    /// zeros (an earlier one is gone too), and so is its last MiB (an earlier backup table),
    /// though the rest of the image may be there.
    #[test]
    fn a_stopped_write_leaves_no_partition_table() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("disk");
        let (img, _) = image(3 * HEAD as usize + 12_345);
        let disk = 6 * TAIL;
        for upto in [0, 10, HEAD as usize - 1, HEAD as usize + 1, img.len() - 1, img.len()] {
            std::fs::write(&p, vec![0xa5u8; disk as usize]).unwrap();
            let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
            let r = put(&mut f, img.len() as u64, disk, source(&img, 65_536, upto, Err(anyhow::anyhow!(DAMAGED))), |_| {});
            assert!(r.is_err(), "{upto}");
            let got = std::fs::read(&p).unwrap();
            assert!(got[..HEAD as usize].iter().all(|&x| x == 0), "no partition table, stopped at {upto}");
            assert!(got[(disk - TAIL) as usize..].iter().all(|&x| x == 0), "no backup table at the end, stopped at {upto}");
            if upto > HEAD as usize {
                assert!(got[HEAD as usize..upto] == img[HEAD as usize..upto], "the rest as far as it came");
            }
            assert!(got[img.len()..(disk - TAIL) as usize].iter().all(|&x| x == 0xa5), "between, left");
        }
        // A disk exactly the image's size: its old backup table, at its end, is gone before the
        // image reaches it.
        let disk = img.len() as u64;
        for upto in [0, 10, HEAD as usize + 1, img.len() - TAIL as usize - 1] {
            std::fs::write(&p, vec![0xa5u8; disk as usize]).unwrap();
            let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
            assert!(put(&mut f, disk, disk, source(&img, 65_536, upto, Err(anyhow::anyhow!(DAMAGED))), |_| {}).is_err());
            let got = std::fs::read(&p).unwrap();
            assert!(got[..HEAD as usize].iter().all(|&x| x == 0) && got[(disk - TAIL) as usize..].iter().all(|&x| x == 0), "stopped at {upto}");
        }
        // And written whole, it is the image.
        std::fs::write(&p, vec![0xa5u8; disk as usize]).unwrap();
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
        put(&mut f, disk, disk, source(&img, 65_536, img.len(), Ok(())), |_| {}).unwrap();
        assert!(std::fs::read(&p).unwrap() == img);
    }

    /// The installer's own copy is checked in the failing direction too: a byte changed, one too
    /// many or too few, is damaged.
    #[test]
    fn a_damaged_copy_is_found() {
        let (img, sha) = image(5 * CHUNK + 3);
        let run = |v: &[u8]| pieces(&mut std::io::Cursor::new(v), img.len() as u64, |_, _| Ok(())).and_then(|(n, h)| whole(n, &h, img.len() as u64, &sha));
        assert!(run(&img).is_ok());
        let mut bad = img.clone();
        bad[2 * CHUNK + 17] ^= 1;
        assert!(run(&bad).is_err(), "a byte changed");
        assert!(run(&img[..img.len() - 1]).is_err(), "short");
        let mut long = img.clone();
        long.push(0);
        assert!(format!("{:#}", run(&long).unwrap_err()).contains("damaged"), "long");
    }

    /// Read back, the disk must hold the image exactly: a byte changed, or less of it, is not kept.
    #[test]
    fn a_disk_that_reads_back_differently_is_refused() {
        let (img, sha) = image(2 * CHUNK + 99);
        let b = img.len() as u64;
        assert!(kept(&mut std::io::Cursor::new(&img), b, &sha, |_| {}).unwrap());
        let mut bad = img.clone();
        bad[CHUNK + 1] ^= 0x80;
        assert!(!kept(&mut std::io::Cursor::new(&bad), b, &sha, |_| {}).unwrap());
        assert!(!kept(&mut std::io::Cursor::new(&img[..img.len() - 1]), b, &sha, |_| {}).unwrap());
    }

    /// Another disk with TorqOS's labels is refused; the chosen disk's own are not; and a blkid
    /// that fails other than by finding none is not taken for "none".
    #[test]
    fn labels_elsewhere_are_refused_and_a_failed_probe_is_not_none() {
        let d = Disk { name: "sda".into(), bytes: 1 << 30, model: String::new(), bus: "SATA", parts: vec!["sda1".into(), "sda2".into()], seq: "3".into(), dev: "8:0".into() };
        assert!(not_elsewhere(&d, "/dev/sda2\n", ROOT_LABEL).is_ok());
        assert!(not_elsewhere(&d, "", ROOT_LABEL).is_ok());
        assert!(not_elsewhere(&d, "/dev/sda1\n/dev/nvme0n1p1\n", BOOT_LABEL).is_err());
        assert!(not_elsewhere(&d, "/dev/sdab1\n", BOOT_LABEL).is_err(), "a name that starts like the disk's is another");
        assert_eq!(labelled(Some(0), "/dev/sdb2\n", "", ROOT_LABEL).unwrap(), "/dev/sdb2\n");
        assert_eq!(labelled(Some(2), "", "", ROOT_LABEL).unwrap(), "");
        for (code, out) in [(Some(1), ""), (Some(4), ""), (Some(8), ""), (None, ""), (Some(2), "/dev/sdb2\n")] {
            assert!(labelled(code, out, "blkid: error", ROOT_LABEL).is_err(), "{code:?} {out:?}");
        }
    }

    /// Opened again by name after the write, a disk must be the one chosen: same device number,
    /// diskseq and size; a partition must be one of it.
    #[test]
    fn a_disk_replaced_under_its_name_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 1_000_000, "a", &[("sda1", "8:1")], "8:0");
        disk(sys, "pci0000:00/0000:00:1f.2/ata2/host1/target1:0:0/1:0:0:0", "sdb", 1_000_000, "b", &[("sdb1", "8:17")], "8:16");
        let listed = disks(sys, &BTreeSet::new()).unwrap();
        let sda = &listed[0];
        assert!(same_disk(sda, "8:0", sys).is_ok());
        assert!(same_disk(sda, "8:16", sys).is_err(), "another device");
        assert!(part_of(sda, "8:1", sys).is_ok());
        assert!(part_of(sda, "8:17", sys).is_err(), "another disk's partition");
        assert!(part_of(sda, "8:0", sys).is_err(), "the disk is not its own partition");
        // Unplugged and another plugged in at the same number: a new diskseq; or another size.
        std::fs::write(sys.join("block/sda/diskseq"), "99\n").unwrap();
        assert!(same_disk(sda, "8:0", sys).is_err());
        assert!(part_of(sda, "8:1", sys).is_err());
        std::fs::write(sys.join("block/sda/diskseq"), format!("{}\n", sda.seq)).unwrap();
        std::fs::write(sys.join("block/sda/size"), "2000000\n").unwrap();
        assert!(same_disk(sda, "8:0", sys).is_err());
    }

    /// The installer offers the machine's disks — never its own medium, a disk in use, a device
    /// that is not a disk, a read-only or empty one.
    #[test]
    fn only_disks_nothing_holds() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 3_907_029_168, "WDC WD20EZAZ", &[("sda1", "8:1"), ("sda2", "8:2")], "8:0");
        disk(sys, "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0", "sdb", 60_000_000, "Cruzer", &[("sdb1", "8:17")], "8:16");
        disk(sys, "pci0000:00/0000:01:00.0/nvme/nvme0/nvme0n1", "nvme0n1", 1_000_215_216, "Samsung SSD 980 PRO", &[], "259:0");
        disk(sys, "virtual/block/loop0", "loop0", 2_000_000, "", &[], "7:0");
        disk(sys, "pci0000:00/0000:00:1f.2/ata2/host1/target1:0:0/1:0:0:0", "sdc", 1_000_000, "read-only", &[], "8:32");
        std::fs::write(sys.join("block/sdc/ro"), "1\n").unwrap();
        disk(sys, "pci0000:00/0000:00:1f.2/ata3/host2/target2:0:0/2:0:0:0", "sdd", 0, "empty reader", &[], "8:48");
        // The installer runs from the USB stick: its partition mounted at /iso, the store a loop
        // device on a file there, the root in RAM.
        let mountinfo = "\
22 1 0:21 / / rw - tmpfs tmpfs rw\n\
25 22 8:17 / /iso ro - iso9660 /dev/sdb1 ro\n\
26 22 7:0 / /nix/.ro-store ro - squashfs /dev/loop0 ro\n\
27 22 0:5 / /proc rw - proc proc rw\n";
        let busy = busy(sys, mountinfo);
        assert_eq!(busy, ["loop0", "sdb"].iter().map(|s| s.to_string()).collect(), "the medium's disk, not its partition's name");
        let d = disks(sys, &busy).unwrap();
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["nvme0n1", "sda"], "sorted; not the stick, loop, read-only or empty");
        assert_eq!((d[0].bus, d[0].parts.len(), d[0].bytes, d[0].dev.as_str()), ("NVMe", 0, 1_000_215_216 * 512, "259:0"));
        assert_eq!((d[1].bus, d[1].parts.clone(), d[1].model.as_str(), d[1].dev.as_str()), ("SATA", vec!["sda1".to_string(), "sda2".to_string()], "WDC WD20EZAZ", "8:0"));
        // Nothing mounted: the stick is offered too, as USB.
        let all = disks(sys, &BTreeSet::new()).unwrap();
        assert_eq!(all.iter().find(|d| d.name == "sdb").map(|d| d.bus), Some("USB"));
    }

    /// A disk that is part of something — an LVM volume group, a RAID array, through the disk or a
    /// partition — is not offered, mounted or not; nor an NVMe disk's hidden second path.
    #[test]
    fn a_disk_that_is_part_of_something_or_hidden_is_not_offered() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 1_000_000, "lvm", &[("sda1", "8:1")], "8:0");
        disk(sys, "pci0000:00/0000:00:1f.2/ata2/host1/target1:0:0/1:0:0:0", "sdb", 1_000_000, "raid", &[], "8:16");
        disk(sys, "pci0000:00/0000:00:1f.2/ata3/host2/target2:0:0/2:0:0:0", "sdc", 1_000_000, "free", &[], "8:32");
        disk(sys, "pci0000:00/0000:01:00.0/nvme/nvme0/nvme0c0n1", "nvme0c0n1", 1_000_000, "a path", &[], "259:1");
        std::fs::create_dir_all(sys.join("block/sda/sda1/holders/dm-0")).unwrap();
        std::fs::create_dir_all(sys.join("block/sdb/holders/md0")).unwrap();
        std::fs::write(sys.join("block/nvme0c0n1/hidden"), "1\n").unwrap();
        let d = disks(sys, &BTreeSet::new()).unwrap();
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["sdc"]);
    }

    /// A disk under a device-mapper or RAID device something mounts is in use too.
    #[test]
    fn a_disk_under_a_mounted_device_is_busy() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        disk(sys, "pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0", "sda", 1_000_000, "a", &[("sda1", "8:1")], "8:0");
        disk(sys, "virtual/block/dm-0", "dm-0", 900_000, "", &[], "253:0");
        std::fs::create_dir_all(sys.join("devices/virtual/block/dm-0/block/dm-0/slaves")).unwrap();
        symlink(sys.join("devices/pci0000:00/0000:00:1f.2/ata1/host0/target0:0:0/0:0:0:0/block/sda/sda1"), sys.join("devices/virtual/block/dm-0/block/dm-0/slaves/sda1")).unwrap();
        let busy = busy(sys, "30 22 253:0 / /mnt rw - ext4 /dev/dm-0 rw\n");
        assert!(busy.contains("sda") && busy.contains("dm-0"), "{busy:?}");
        assert!(disks(sys, &busy).unwrap().is_empty());
    }

    #[test]
    fn names_and_sizes() {
        assert_eq!(partition("sda", 2), "sda2");
        assert_eq!(partition("vda", 1), "vda1");
        assert_eq!(partition("nvme0n1", 2), "nvme0n1p2");
        assert_eq!(partition("mmcblk0", 2), "mmcblk0p2");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(3_400_000_000), "3.4 GB");
        assert_eq!(size(512_110_190_592), "512 GB");
        assert_eq!(size(2_000_398_934_016), "2.0 TB");
        for no in ["loop0", "ram1", "zram0", "sr0", "dm-3", "md127", "fd0", "nbd0", "nullb0", "mmcblk0boot0", "mmcblk0boot1", "mmcblk0rpmb"] {
            assert!(not_a_disk(no), "{no}");
        }
        for yes in ["sda", "vda", "nvme0n1", "mmcblk0", "xvda"] {
            assert!(!not_a_disk(yes), "{yes}");
        }
        let d = Disk { name: "sda".into(), bytes: 2_000_000_000, model: String::new(), bus: "SATA", parts: vec!["sda1".into()], seq: "7".into(), dev: "8:0".into() };
        assert_eq!(describe(&d, 3_000_000_000), "sda          2.0 GB  SATA    (no model name), 1 partition — too small: TorqOS needs 3.0 GB");
    }

    /// The hardware file is NixOS's detection and the image's disk, each as written, in one module.
    #[test]
    fn the_hardware_file() {
        let detected = "# Do not modify this file!\n{ config, lib, pkgs, modulesPath, ... }:\n\n{\n  imports = [ (modulesPath + \"/installer/scan/not-detected.nix\") ];\n  boot.initrd.availableKernelModules = [ \"nvme\" \"xhci_pci\" ];\n}\n";
        let fs = "{ ... }:\n{\n  fileSystems.\"/\" = { device = \"/dev/disk/by-label/nixos\"; fsType = \"ext4\"; };\n}\n";
        let none = Found::default();
        let f = hardware_file(detected, &none, fs).unwrap();
        assert!(f.contains(detected.trim_end()) && f.contains(fs.trim_end()), "{f}");
        assert!(f.find("not-detected.nix").unwrap() < f.find("by-label/nixos").unwrap());
        assert!(f.trim_end().ends_with("];\n}"), "{f}");
        assert!(!f.contains("iwd"), "no Wi-Fi found, none turned on: {f}");
        assert!(hardware_file("  \n", &none, fs).is_err(), "nothing detected");
        assert!(hardware_file("{ ... }: { fileSystems.\"/\" = { }; }", &none, fs).is_err(), "the image's file systems only");
        assert!(hardware_file("{ ... }: { swapDevices = [ ]; }", &none, fs).is_err());
        // Wi-Fi found: iwd on, between the detected hardware and the image's disk.
        let w = hardware_file(detected, &Found { wireless: true }, fs).unwrap();
        let at = w.find("networking.wireless.iwd.enable = true;").expect("iwd on");
        assert!(w.find("not-detected.nix").unwrap() < at && at < w.find("by-label/nixos").unwrap(), "{w}");
    }

    /// What a first boot builds from is copied into the new store only where it is missing, whole under
    /// its own name (links and modes kept), and nothing outside the store is taken.
    #[test]
    fn carried_paths_are_copied_where_missing() {
        if !Path::new(BIN).join("cp").exists() {
            return;
        }
        let from = tempfile::tempdir().unwrap();
        let to = tempfile::tempdir().unwrap();
        let src = from.path().join("nix/store");
        std::fs::create_dir_all(src.join("aaaa-iwd-3.12/bin")).unwrap();
        std::fs::write(src.join("aaaa-iwd-3.12/bin/iwd"), "iwd").unwrap();
        symlink("iwd", src.join("aaaa-iwd-3.12/bin/iwctl")).unwrap();
        std::fs::write(src.join("bbbb-firmware"), "fw").unwrap();
        std::fs::create_dir_all(to.path().join("bbbb-firmware")).unwrap();
        let paths = "/nix/store/aaaa-iwd-3.12\n/nix/store/bbbb-firmware\n\n";
        assert_eq!(carry(paths, from.path(), to.path()).unwrap(), 1, "the firmware is there already");
        assert_eq!(std::fs::read_to_string(to.path().join("aaaa-iwd-3.12/bin/iwd")).unwrap(), "iwd");
        assert_eq!(std::fs::read_link(to.path().join("aaaa-iwd-3.12/bin/iwctl")).unwrap(), Path::new("iwd"));
        assert!(to.path().join("bbbb-firmware").is_dir(), "what was there is left as it was");
        assert!(std::fs::read_dir(to.path()).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with('.')), "no copy left half done");
        assert_eq!(carry(paths, from.path(), to.path()).unwrap(), 0, "again: nothing to add");
        for bad in ["/etc/shadow", "/nix/store/", "/nix/store/../x", "/nix/store/a/b", "/nix/store/.hidden"] {
            assert!(carry(bad, from.path(), to.path()).is_err(), "{bad}");
        }
    }

    /// Wi-Fi is found by a wireless interface, or by a PCI device of the wireless class before its driver
    /// is up (its firmware missing); an Ethernet card or nothing at all is no Wi-Fi.
    #[test]
    fn wifi_is_found() {
        let sys = tempfile::tempdir().unwrap();
        let s = sys.path();
        let mk = |p: &str| std::fs::create_dir_all(s.join(p)).unwrap();
        assert_eq!(found(s), Found { wireless: false }, "an empty /sys");
        mk("class/net/lo");
        mk("class/net/enp1s0");
        mk("bus/pci/devices/0000:01:00.0");
        std::fs::write(s.join("bus/pci/devices/0000:01:00.0/class"), "0x020000\n").unwrap();
        assert_eq!(found(s), Found { wireless: false }, "Ethernet only");
        mk("bus/pci/devices/0000:c3:00.0");
        std::fs::write(s.join("bus/pci/devices/0000:c3:00.0/class"), "0x028000\n").unwrap();
        assert_eq!(found(s), Found { wireless: true }, "the MT7925 by its PCI class");
        std::fs::remove_dir_all(s.join("bus/pci/devices/0000:c3:00.0")).unwrap();
        mk("class/net/wlan0/wireless");
        assert_eq!(found(s), Found { wireless: true }, "a USB Wi-Fi stick by its interface");
    }
}
