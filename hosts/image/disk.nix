# The image's disk: NixOS's image builder (through its option: ADR-039 keeps nixpkgs' tree out of reach)
# makes it whole, boot loader and all, on ext4, the only file
# system it partitions; a second step copies that root onto btrfs and keeps its EFI partition as it
# is. btrfs for snapshots of data (ADR-038). The layout is hosts/image/hardware-configuration.nix's.
{ config, lib, pkgs, ... }:

let
  built = config.system.build.images.qemu-efi;
  vm = pkgs.vmTools.override {
    rootModules = [ "virtio_pci" "virtio_mmio" "virtio_blk" "virtio_balloon" "virtio_rng" "ext4" "virtiofs" "crc32c" "btrfs" "vfat" "nls_cp437" "nls_iso8859-1" ];
  };
in
{
  # The builder's module says ext4 for the disk it makes; the system on it says btrfs, as it will be.
  image.modules.qemu-efi = { lib, ... }: { fileSystems."/".fsType = lib.mkForce "btrfs"; };

  system.build.torqDisk = vm.runInLinuxVM (pkgs.runCommand "torqos-disk"
    {
      memSize = 4096;
      nativeBuildInputs = [ pkgs.btrfs-progs pkgs.e2fsprogs pkgs.util-linux pkgs.qemu-utils pkgs.coreutils ];
      preVM = ''
        ${pkgs.qemu-utils}/bin/qemu-img convert -f qcow2 -O raw ${built}/*.qcow2 built.raw
        src=built.raw
        # Room for btrfs's metadata, more than ext4's.
        size=$(( $(stat -c %s "$src") * 5 / 4 + (1024 << 20) ))
        ${pkgs.qemu-utils}/bin/qemu-img create -f raw disk.raw $size
        diskImage=disk.raw
        QEMU_OPTS="$QEMU_OPTS -drive file=$src,if=virtio,format=raw,readonly=on,cache=unsafe"
      '';
      postVM = ''
        mkdir -p $out
        ${pkgs.qemu-utils}/bin/qemu-img convert -f raw -O qcow2 disk.raw $out/torqos.qcow2
      '';
    } ''
    set -euo pipefail
    # The drive named by runInLinuxVM is vda; the built image vdb, read only.
    esp_start=$(cat /sys/class/block/vdb1/start)
    esp_size=$(cat /sys/class/block/vdb1/size)
    # The ESP as built; the record's own (1 GiB, before the root, which grows into the rest); the root.
    printf 'label: gpt\nstart=%s, size=%s, type=U\nsize=1GiB, type=L\ntype=L\n' "$esp_start" "$esp_size" | sfdisk --quiet /dev/vda
    udevadm settle 2>/dev/null || true
    partx -u /dev/vda 2>/dev/null || true
    dd if=/dev/vdb1 of=/dev/vda1 bs=1M status=none
    mkfs.ext4 -q -L torq -m 0 /dev/vda2
    mkfs.btrfs -q -L nixos /dev/vda3
    mkdir -p /top /new /old
    mount -t btrfs /dev/vda3 /top
    for s in @ @home @var-lib @snapshots; do btrfs -q subvolume create /top/$s; done
    umount /top
    mount -t btrfs -o subvol=@ /dev/vda3 /new
    mkdir -p /new/home /new/var/lib /new/.snapshots
    mount -t btrfs -o subvol=@home /dev/vda3 /new/home
    mount -t btrfs -o subvol=@var-lib /dev/vda3 /new/var/lib
    mkdir -p /new/var/lib/torq
    mount -t ext4 /dev/vda2 /new/var/lib/torq
    mount -t btrfs -o subvol=@snapshots /dev/vda3 /new/.snapshots
    chmod 700 /new/.snapshots
    mount -t ext4 -o ro,noload /dev/vdb2 /old
    cp -a /old/. /new/
    chmod 755 /new/var/lib/torq
    umount /old /new/.snapshots /new/var/lib/torq /new/var/lib /new/home /new
  '');
}
