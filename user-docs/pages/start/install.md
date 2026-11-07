# Install

TorqOS is installed from an image: download it, check it, boot it, and answer a few questions.
Nothing needs to be built.

## 1. Download

Get the image for your platform from the
[latest release](https://github.com/netstacks/torqos-releases/releases/latest):

| File | For |
|---|---|
| `torqos-x86_64-linux.qcow2` | QEMU/KVM, Proxmox VE, libvirt (x86_64) |
| `torqos-aarch64-linux.qcow2` | UTM on Apple silicon, QEMU (aarch64) |
| `torqos-x86_64-linux.vmdk.gz` | VMware ESXi (x86_64) — `gunzip` it first |
| `torqos-aarch64-linux.vmdk.gz` | VMware on arm64 — `gunzip` it first |

From the same release, also download `SHA256SUMS`, `SHA256SUMS.sig` and `allowed_signers`.

The download is about 730 MB compressed; an installed system is about 1.5 GB.

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

    Use `torqos-x86_64-linux.qcow2` (or the aarch64 image on an ARM host) as the VM's disk, with UEFI
    firmware. With libvirt, for example:

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

=== "UTM (Apple silicon)"

    1. Create a new virtual machine: **Virtualize → Linux**, and skip the installation ISO.
    2. Remove the empty drive UTM creates, and import `torqos-aarch64-linux.qcow2` as the VM's
       drive.
    3. Keep UTM's default display and UEFI boot, and start the VM.

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
