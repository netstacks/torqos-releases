# NixOS VM test: the ISO installer (ADR-031), from a USB stick to the installed machine's first
# boot. The installer starts on its console from a USB stick in a UEFI machine with one empty disk;
# it offers that disk and never its own stick; a wrong name writes nothing; the right one writes the
# image, reads it back and leaves the machine's detected hardware on it. Then the machine starts from
# that disk alone: its root has grown to the disk, first-boot setup is on its console, and the
# hardware file setup will take is valid NixOS, with the image's file systems.
#
# Real throughout — the ISO, the installer, the image's system and disk layout — except that both
# carry NixOS's test instrumentation, the backdoor a test reads and types through (as NixOS's own
# ISO tests do). Both are started as plain QEMU machines with UEFI firmware, not as test nodes:
# what boots is the ISO and the disk, nothing else.
{ pkgs, self }:

let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  instrumented = { modulesPath, ... }: {
    imports = [ (modulesPath + "/testing/test-instrumentation.nix") ];
    # NixOS's test instrumentation gives root an empty password; TorqOS asserts root has none.
    users.users.root.hashedPasswordFile = lib.mkForce null;
  };

  image = self.lib.mkSystem {
    inherit system;
    modules = [ ../hosts/image { torq.setup.source = self.sourceInfo; } instrumented ];
  };
  payload = import ../hosts/installer/payload.nix { inherit pkgs; image = image.config.system.build.torqDisk; };
  installer = self.lib.mkSystem {
    inherit system;
    modules = [ ../hosts/installer { torq.installer.payload = payload; } instrumented ];
  };
  iso = "${installer.config.system.build.isoImage}/iso/${installer.config.image.fileName}";

  # The image's file systems, as the hardware file must name them (hosts/image/hardware-configuration.nix).
  want = builtins.toJSON {
    mounts = builtins.mapAttrs (_: f: { inherit (f) device fsType options autoResize neededForBoot; }) image.config.fileSystems;
    # Grown by systemd-repart in the initrd (not growpart, which needs Python: ADR-032, amended).
    grow = { inherit (image.config.boot.initrd.systemd.repart) enable; partitions = image.config.systemd.repart.partitions; growpart = image.config.boot.growPartition; layout = image.config.boot.initrd.systemd.services.systemd-repart.serviceConfig.ExecCondition; };
  };

  qemu = "${pkgs.qemu_test}/bin/qemu-system-${pkgs.stdenv.hostPlatform.qemuArch}";
in
pkgs.testers.runNixOSTest {
  name = "torq-installer";
  nodes = { };
  globalTimeout = 3600;

  testScript = ''
    import json, os, re, shlex, shutil, subprocess

    tmp = os.environ.get("TMPDIR", "/tmp")
    disk = os.path.join(tmp, "disk.qcow2")
    subprocess.run(["${pkgs.qemu_test}/bin/qemu-img", "create", "-f", "qcow2", disk, "12G"], check=True)
    image_bytes = int(open("${payload}/size").read())

    def machine(name, *devices):
        # UEFI, with firmware variables of its own (writable, as a real machine's are), and no
        # network card: nothing to try starting from but the disks given.
        variables = os.path.join(tmp, f"{name}-vars.fd")
        shutil.copy("${pkgs.OVMF.variables}", variables)
        os.chmod(variables, 0o644)
        return create_machine(" ".join([
            "${qemu}", "-m 3072", "-smp 2", "-vga std", "-nic none",
            "-drive if=pflash,format=raw,unit=0,readonly=on,file=${pkgs.OVMF.firmware}",
            f"-drive if=pflash,format=raw,unit=1,file={variables}",
            *devices,
        ]), name=name)

    # The disk to install to, started from second while the stick is in, first once it is not.
    def target(boot):
        return f"-drive id=disk,file={disk},if=none,format=qcow2 -device virtio-blk-pci,drive=disk,bootindex={boot}"

    # The stick the installer starts from — writable, as a stick is (writes go to a scratch copy) —
    # and a second copy of it, which nothing mounts: the installer's own medium too, known only by its
    # label (as the one it started from is, copied to RAM).
    usb = [
        "-device qemu-xhci,id=xhci",
        "-drive id=stick,file=${iso},if=none,format=raw,snapshot=on -device usb-storage,bus=xhci.0,drive=stick,removable=on,bootindex=0",
        "-drive id=spare,file=${iso},if=none,format=raw,snapshot=on -device usb-storage,bus=xhci.0,drive=spare,removable=on",
    ]

    def on_console(m, text, timeout=300):
        try:
            m.wait_until_succeeds(f"grep -aqF {shlex.quote(text)} /dev/vcs1", timeout=timeout)
        except Exception:
            # What the screen shows instead, and what the installer (or setup) and the disks did:
            # each on its own, so one that fails or hangs hides none of the others.
            for c in ["fgconsole; tr -s ' \\0' ' ' < /dev/vcs1",
                      "systemctl status --no-pager torq-install torq-setup",
                      "journalctl -b --no-pager -o short-monotonic -u torq-install -u torq-setup -u systemd-repart",
                      "journalctl -b --no-pager -o short-monotonic -k --grep 'usb|sd[a-z]|vd[a-z]|I/O|squashfs|Console|drm' | tail -40",
                      "lsblk"]:
                print(m.execute(c, timeout=60)[1])
            raise

    def answer(m, text):
        m.send_chars(text + "\n")

    # The console's rows: /dev/vcs1 is its cells, no line breaks, so cut at its width.
    def rows(m):
        cols = int(m.succeed("stty -F /dev/tty1 size").split()[1])
        s = m.succeed("cat /dev/vcs1").replace("\0", " ")
        return [s[i:i + cols].rstrip() for i in range(0, len(s), cols)]

    installer = machine("installer", *usb, target(1))

    with subtest("the installer starts on its console, offering the empty disk and not its own sticks"):
        installer.start()
        on_console(installer, "Welcome to the TorqOS installer")
        # After the installer checks its own copy of the image, read from the stick (slow, emulated USB).
        # (Or its refusal, which fails the test now rather than at the timeout.)
        installer.wait_until_succeeds("grep -aqF -e 'Install TorqOS on' -e 'is damaged' /dev/vcs1", timeout=1200)
        on_console(installer, "Install TorqOS on", timeout=1)
        # The list's rows: "  1  vda   12 GB  virtio  (no model name), no partitions".
        offered = [m.groups() for r in rows(installer) if (m := re.match(r"\s+\d+\s+(\S+)\s+\S+ [kMGT]?B\s+(\S+)", r))]
        sticks = installer.succeed("lsblk -ndo NAME,TRAN | awk '$2 == \"usb\" {print $1}'").split()
        assert len(sticks) == 2, installer.succeed("lsblk")
        assert offered == [("vda", "virtio")], f"offered {offered}; the sticks are {sticks}:\n" + "\n".join(rows(installer))
        # A graphics driver taking the console over late changes its size: the installer asks again,
        # with its welcome and the list (setup.rs `asked`, with the installer's heading). On purpose.
        installer.succeed("printf '\\033[H\\033[2J' > /dev/tty1 && stty -F /dev/tty1 rows 30 cols 100")
        on_console(installer, "Welcome to the TorqOS installer")
        on_console(installer, "Install TorqOS on")
        assert any(re.match(r"\s+1\s+vda\s", r) for r in rows(installer)), "\n".join(rows(installer))
        # Nobody can log in, and nothing listens, TCP or UDP, but on the machine itself
        # (systemd-resolved's stub).
        installer.succeed("systemctl is-active torq-install")
        installer.fail("systemctl is-active getty@tty1")
        installer.succeed("passwd -S root | grep -q ' L '")
        out = installer.succeed("ss -Htlnu")
        assert all(l.split()[4].startswith(("127.", "[::1]", "[::ffff:127.")) for l in out.splitlines()), out

    with subtest("a wrong name writes nothing"):
        answer(installer, "1")
        on_console(installer, "Type its name, vda")
        answer(installer, "sda")
        on_console(installer, "Not confirmed: nothing was written.")
        # Not a byte of the disk: written and cleared again would leave its start zeros, not the rest.
        installer.succeed("cmp -n $(blockdev --getsize64 /dev/vda) /dev/vda /dev/zero", timeout=600)

    with subtest("the right name writes the image, reads it back and leaves the detected hardware"):
        on_console(installer, "Starting over.")
        answer(installer, "1")
        # The question asked again, not the first asking still on the screen: it is the last row.
        retry(lambda _: "Type its name, vda" in [r for r in rows(installer) if r.strip()][-1], timeout_seconds=60)
        answer(installer, "vda")
        # Mid-write, the disk has no partition table: the image's first MiB is written last, so
        # stopped now (a power cut) the disk is not a half-written system that starts.
        installer.wait_until_succeeds("grep -aqF 'Writing TorqOS to vda' /dev/vcs1", timeout=600)
        installer.fail("grep -aqF 'Checking vda' /dev/vcs1")
        installer.succeed("cmp -n 1048576 /dev/vda /dev/zero")
        # And no key stops the machine mid-write: Ctrl-Alt-Del, once or seven times quickly; the
        # power key (the ACPI button QEMU's monitor presses), which logind would answer with a
        # poweroff that stops the installer.
        assert installer.succeed("systemctl show -P LoadState ctrl-alt-del.target").strip() == "masked"
        for _ in range(8):
            installer.send_key("ctrl-alt-delete")
        # Every key and the lid, pressed or held (modules/installer.nix): eleven, all ignored.
        assert installer.succeed("grep -c '^Handle' /etc/systemd/logind.conf").strip() == "11"
        assert installer.succeed("grep -c '^Handle[A-Za-z]*=ignore$' /etc/systemd/logind.conf").strip() == "11"
        installer.send_monitor_command("system_powerdown")
        # The press reached logind, which ignored it: not a key that never arrived.
        installer.wait_until_succeeds("journalctl -b -u systemd-logind | grep -q 'Power key pressed'", timeout=60)
        on_console(installer, "TorqOS is on vda.", timeout=1200)
        installer.succeed("systemctl is-active torq-install")
        installer.succeed("test \"$(systemctl is-system-running || true)\" != stopping")
        # The installer read the whole image back from the disk before it mounted the root to leave
        # the hardware file there (which changes the root): what comes before the root, the
        # partition tables and the boot partition, is still the image's byte for byte.
        installer.succeed(
            "cmp -n $(( $(cat /sys/class/block/vda2/start) * 512 )) /dev/vda <(zstd -dcq \"$(readlink -f /etc/torq/installer/image.raw.zst)\")"
            " && test $(( $(cat /sys/class/block/vda2/start) + $(cat /sys/class/block/vda2/size) )) -le $(( "
            + str(image_bytes) + " / 512 ))"
        )
        # The ISO has no /mnt: a mount point of the test's own.
        installer.succeed("mkdir -p /tmp/root && mount -o ro LABEL=torq /tmp/root")
        hw = installer.succeed("cat /tmp/root/install/hardware-configuration.nix")
        installer.succeed("umount /tmp/root")
        assert "qemu-guest.nix" in hw and "by-label/nixos" in hw, hw
        # The boot menu the disk starts with cannot have its entries edited (modules/security.nix;
        # a kernel command line typed at boot is a root shell): systemd-boot's `editor 0`. Until
        # the EFI-partition test (ADR-033) checks the menu itself.
        installer.succeed("mkdir -p /tmp/esp && mount -o ro /dev/vda1 /tmp/esp")
        installer.succeed("grep -qx 'editor 0' /tmp/esp/loader/loader.conf")
        installer.succeed("umount /tmp/esp")
        # The ESP's and the record's partitions as the image left them: where each starts, its size, type
        # and name; the record's of TorqOS's own type (hosts/image/disk.nix), which repart has no
        # definition for.
        before = installer.succeed("sfdisk --dump /dev/vda | grep -E '^/dev/vda[12] '").strip()
        assert "type=79312C32-17CC-4864-8A4C-5A6D06522000" in before.splitlines()[1], before
        installer.shutdown()

    installed = machine("installed", target(0))

    with subtest("the machine starts from the disk: grown to it, with first-boot setup on its console"):
        installed.start()
        on_console(installed, "Welcome to TorqOS. This machine has no configuration yet")
        # Grown at boot (systemd-repart in the initrd, then the file system's autoResize), from the
        # image's 6 GB: the root's partition, the last, and only it.
        installed.wait_until_succeeds("test $(df --output=size -B1 / | tail -1) -gt 10000000000", timeout=120)
        # repart numbers partitions from 0: vda3 is its 2. It ran (the layout check let it), grew the
        # root, and neither of the others.
        installed.succeed("journalctl -b --no-pager --identifier systemd-repart --grep 'Growing existing partition 2\\.'")
        installed.fail("journalctl -b --no-pager --identifier systemd-repart --grep 'existing partition [01]\\.'")
        assert installed.succeed("sfdisk --dump /dev/vda | grep -E '^/dev/vda[12] '").strip() == before, "the ESP's and the record's partitions are untouched"
        assert installed.succeed("test $(( $(cat /sys/class/block/vda3/start) + $(cat /sys/class/block/vda3/size) )) -gt $(( 11 << 21 )) && echo ok").strip() == "ok"
        installed.fail("systemctl cat growpart.service")
        # One btrfs file system, each subvolume where it belongs, and the record not under /var/lib's.
        for mount, subvol in [("/", "@"), ("/home", "@home"), ("/var/lib", "@var-lib"), ("/.snapshots", "@snapshots")]:
            got = installed.succeed(f"findmnt -no FSTYPE,OPTIONS --mountpoint {mount}").strip()
            assert got.startswith("btrfs") and f"subvol=/{subvol}" in got, (mount, got)
        assert installed.succeed("stat -c %a /.snapshots").strip() == "700"
        assert installed.succeed("stat -c %a /var/lib/torq").strip() == "755"
        # The record on a file system of its own: room whoever fills the rest (the audit of M3, M2).
        assert installed.succeed("findmnt -no FSTYPE,SOURCE --mountpoint /var/lib/torq").split()[0] == "ext4"
        assert installed.succeed("findmnt -no SOURCE --mountpoint /var/lib/torq") != installed.succeed("findmnt -no SOURCE --mountpoint /var/lib")
        # What the image carried came across: the system, the users' database, the detected hardware.
        installed.succeed("test -e /nix/var/nix/profiles/system && test -e /var/lib/nixos/uid-map")
        installed.succeed("test -s /var/lib/torq/install/hardware-configuration.nix")
        # On by themselves on this disk, and the record not in one (the real layout, not a test's).
        installed.succeed("sh -c \"$(sed -n 's/.*\"snapshot\":\"\([^\"]*\) {label}\".*/\\1/p' /etc/torq/broker.json) 123\"")
        installed.succeed("test -d /.snapshots/123/home && test -d /.snapshots/123/var-lib/nixos")
        assert installed.succeed("ls -A /.snapshots/123/var-lib/torq").strip() == "", "no snapshot holds the record"

    with subtest("the hardware setup takes is valid NixOS, with the image's file systems"):
        got = installed.succeed(
            "nix-instantiate --eval --strict --json -E "
            + shlex.quote(
                'let c = (import ${self.inputs.nixpkgs}/nixos/lib/eval-config.nix { system = "${system}"; '
                'modules = [ /var/lib/torq/install/hardware-configuration.nix { boot.loader.grub.enable = false; system.stateVersion = "26.05"; } ]; }).config; in '
                '{ mounts = builtins.mapAttrs (_: f: { inherit (f) device fsType options autoResize neededForBoot; }) c.fileSystems; '
                'grow = { inherit (c.boot.initrd.systemd.repart) enable; partitions = c.systemd.repart.partitions; growpart = c.boot.growPartition; layout = c.boot.initrd.systemd.services.systemd-repart.serviceConfig.ExecCondition; }; virtio = builtins.elem "virtio_pci" c.boot.initrd.availableKernelModules; }'
            )
        )
        got = json.loads(got)
        assert got.pop("virtio"), "the detected hardware names this machine's disk controller"
        assert got == json.loads(${builtins.toJSON want}), got

    with subtest("change #1's system, with the detected hardware, evaluates as setup builds it"):
        # The config repo setup writes (setup.rs `files`), from the image's template and lock, the
        # owner `alice`, and the hardware file setup takes (`hardware_file`): evaluated through
        # TorqOS's own mkSystem, its assertions and fence included, with no network.
        installed.succeed(
            "rm -rf /tmp/cfg && cp -r /etc/torq/image/source/templates/config /tmp/cfg && chmod -R u+w /tmp/cfg"
            " && cp /etc/torq/image/flake.lock /var/lib/torq/install/hardware-configuration.nix /tmp/cfg/"
            " && printf '{\"user\": \"alice\"}\\n' > /tmp/cfg/nixos/torq.json"
            " && sed -i -e 's|system = \"aarch64-linux\";|system = \"${system}\";|'"
            " -e \"s|torq.url = \\\"[^\\\"]*\\\";|torq.url = \\\"path:$(readlink -f /etc/torq/image/source)\\\";|\" /tmp/cfg/flake.nix"
        )
        installed.succeed("grep -q 'path:/nix/store/' /tmp/cfg/flake.nix && grep -q 'system = \"${system}\"' /tmp/cfg/flake.nix")
        installed.succeed("nix eval --offline --raw /tmp/cfg#nixosConfigurations.torq.config.system.build.toplevel.drvPath")
        installed.shutdown()
  '';
}
