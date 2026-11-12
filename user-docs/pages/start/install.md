# Install

TorqOS is installed from an image: download it, check it, boot it, and answer a few questions.
Nothing needs to be built.

## 1. Download

Get the image for your platform from the
[latest release](https://github.com/torqosorg/torqos-releases/releases/latest):

| File | For |
|---|---|
| `torqos-x86_64-linux.qcow2` | QEMU/KVM, Proxmox VE, libvirt |
| `torqos-x86_64-linux.vmdk.gz` | VMware ESXi: `gunzip` it first |
| `torqos-x86_64-linux.iso` | A real machine: the installer, written to a USB stick |

TorqOS is built and tested for x86_64 only. The `v0.3.x` releases have no aarch64 images (earlier
releases do, but they are older and are not tested now).

From the same release, also download `SHA256SUMS`, `SHA256SUMS.sig` and `allowed_signers`.

An installed system is about 1.2 GB (a new machine's system is held under a budget of 1280 MB).

## 2. Check the download

You need only OpenSSH and coreutils. In the folder with the files:

```sh
ssh-keygen -Y verify -f allowed_signers -I torqos-release -n file -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
```

The first command should report:

```text
Good "file" signature for torqos-release with ED25519 key SHA256:…
```

Compare that fingerprint with the one in the release notes, so you are not trusting the key only
because it came with the download. The second command should report `OK` for your image.

!!! tip "On macOS"
    Use `shasum -a 256 -c --ignore-missing SHA256SUMS` instead of `sha256sum`.

## 3. Boot it

The image is a single EFI disk. Whatever your hypervisor:

- **Boot with UEFI** firmware, with **Secure Boot off**.
- **Give the VM a display.** First-boot setup runs on the first text console; a VM with only a
  serial console shows login prompts with no account to log in to.
- **Make the disk bigger** than the image if you like (the image is about 6 GB). The root file system
  grows to fill the disk at boot.

=== "QEMU / KVM / libvirt"

    Use `torqos-x86_64-linux.qcow2` as the VM's disk, with UEFI firmware. With libvirt, for example:

    ```sh
    qemu-img resize torqos-x86_64-linux.qcow2 20G
    virt-install --name torqos --import --boot uefi \
      --disk torqos-x86_64-linux.qcow2 --osinfo linux2022 \
      --memory 4096 --vcpus 2 --graphics vnc
    ```

=== "Proxmox VE"

    1. Create a VM with **BIOS: OVMF (UEFI)** and no installation media. If Proxmox offers to
       add an EFI disk with **pre-enrolled keys**, untick that option (it turns on Secure Boot).
    2. Copy the qcow2 to the Proxmox host and import it as the VM's disk:
       ```sh
       qm disk import <vmid> torqos-x86_64-linux.qcow2 <storage>
       ```
    3. In the VM's **Hardware**, attach the imported disk, resize it if you want more space, and
       make it first in **Options → Boot Order**.
    4. Start the VM and open its console.

=== "Real hardware (ISO)"

    The ISO starts on UEFI machines only, with Secure Boot off.

    1. Write `torqos-x86_64-linux.iso` to a USB stick of 2 GB or more, as it is (not by copying the
       file onto the stick). On Linux:
       ```sh
       sudo dd if=torqos-x86_64-linux.iso of=/dev/sdX bs=4M conv=fsync
       ```
       Check the device name first: everything on the stick is erased. Other tools that write a
       disk image raw (balenaEtcher, Rufus in DD mode) work too.
    2. Start the machine from the stick, using its firmware's boot menu. The console shows the
       installer: no login, nothing listening on the network.
    3. The installer lists the disks it can use (whole disks, at least the image's size, never the
       stick it started from), with each one's size and what is on it. Choose one, then type its name
       again (`nvme0n1`, `sda`). **Everything on that disk is erased.** Nothing is written before
       you type the name.
    4. It writes TorqOS to the disk, reads it back to check it, and detects the machine's hardware.
    5. Remove the stick and restart. First-boot setup runs on the console.

    Not yet possible: installing beside another system or onto one partition, disk encryption, and
    machines with BIOS firmware only. TorqOS has been installed on one physical machine so far, the
    owner's laptop.

=== "VMware ESXi"

    1. `gunzip torqos-x86_64-linux.vmdk.gz`, then upload the `.vmdk` to a datastore
       (**Storage → Datastore browser → Upload**).
    2. In the ESXi shell (SSH to the host), in that datastore's folder, copy it into a disk of its
       own and delete the upload:
       ```sh
       vmkfstools -i torqos-x86_64-linux.vmdk torqos.vmdk -d thin
       ```
    3. Create a VM: guest OS **Linux**, **Other Linux (64-bit)** (the newest kernel offered).
       Remove the new disk it proposes and add an existing disk, `torqos.vmdk`, growing it to the
       size you want. In **VM Options → Boot Options**, set firmware to **EFI** with Secure Boot off.
       Any of ESXi's disk controllers (PVSCSI, LSI Logic, SATA, NVMe) and network cards (VMXNET3,
       E1000e) work.
    4. Power it on.

    Tested on ESXi 8.0 U3.

The image has no user account, `root` is locked, SSH is off and nothing listens on the network.
Instead of a login prompt, the console shows first-boot setup.

## 4. Answer setup's questions

Continue with [First-boot setup](first-boot.md).
