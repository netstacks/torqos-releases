{
  description = "TorqOS — a thin, CLI-only OS built on NixOS, whose configuration is the device";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    # TODO(M4): lanzaboote for owner-key Secure Boot + measured boot.
    # lanzaboote = {
    #   url = "github:nix-community/lanzaboote";
    #   inputs.nixpkgs.follows = "nixpkgs";
    # };
  };

  outputs = { self, nixpkgs, ... }:
    let
      lib = nixpkgs.lib;
      linuxSystems = [ "x86_64-linux" "aarch64-linux" ];
      allSystems = linuxSystems ++ [ "aarch64-darwin" ];
      forSystems = systems: f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # The image (packages.image), in one disk format: `ext` its file name's, `args` qemu-img's.
      imageAs = pkgs: ext: args:
        let
          system = pkgs.stdenv.hostPlatform.system;
          whole = self.nixosConfigurations."image-${system}".config.system.build.images.qemu-efi;
        in
        pkgs.runCommand "torqos-${ext}-${system}" { nativeBuildInputs = [ pkgs.qemu-utils ]; } ''
          mkdir $out
          qemu-img convert ${args} ${whole}/*.qcow2 $out/torqos-${system}.${ext}
        '';
    in
    {
      # ── Packages ─────────────────────────────────────────────────────────────
      packages = lib.recursiveUpdate (forSystems allSystems (pkgs: {
        torq-broker = pkgs.callPackage ./broker/package.nix { };
        torq = pkgs.callPackage ./cli/package.nix { };
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.torq-broker;
      })) (forSystems linuxSystems (pkgs: {
        # The TorqOS image (docs/INSTALL.md): a disk to download and boot; first-boot setup on its
        # console. NixOS's image builder makes it whole; qemu-img writes it in the form a platform
        # takes. `nix build .#image`: a qcow2, compressed (a download a third the size or less; UTM,
        # QEMU and Proxmox run it as it is). `nix build .#image-vmdk`: the same disk for VMware ESXi
        # (a sparse VMDK, which `vmkfstools -i` makes the host's own disk).
        image = imageAs pkgs "qcow2" "-c -O qcow2";
        image-vmdk = imageAs pkgs "vmdk" "-O vmdk -o adapter_type=lsilogic";
      }));

      # ── The distribution, as one NixOS module ────────────────────────────────
      nixosModules.default = { pkgs, lib, ... }: {
        imports = [ ./modules ];
        torq.broker.package =
          lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.torq-broker;
        torq.cli.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.torq;
      };

      # Helper used by per-machine config repos (see templates/config).
      # `configDir = ./.;` lets the machine's package lists (system/packages.json, users/<name>/packages.json)
      # be read from the config repo; see modules/packages.nix.
      lib.mkSystem = { system, modules ? [ ], configDir ? null }:
        nixpkgs.lib.nixosSystem {
          modules = [
            self.nixosModules.default
            { nixpkgs.hostPlatform = system; torq.packages.configDir = configDir; }
            # The sources the machine is built from, kept with each system built from them: while a
            # generation exists, its configuration can be evaluated again (the commit check reads
            # the configuration before a change; a rollback rebuilds an older one). Without this a
            # garbage collection deletes them — and a source that cannot be fetched again (the
            # image's own, in the store; a folder that has changed since) is gone for good.
            { system.extraDependencies = [ self.outPath nixpkgs.outPath ]; }
            # The tree's NixOS settings (nixos/*.json), built from the path itself (modules/nixos-settings.nix).
            (import ./modules/nixos-settings.nix configDir)
          ] ++ modules;
        };

      # ── Throwaway dev VMs: nix build .#nixosConfigurations.dev-vm-<arch>.config.system.build.vm
      nixosConfigurations = lib.genAttrs [ "dev-vm-aarch64" "dev-vm-x86_64" ] (name:
        self.lib.mkSystem {
          system = if name == "dev-vm-aarch64" then "aarch64-linux" else "x86_64-linux";
          modules = [ ./hosts/dev-vm ];
        })
      # The image's own system, carrying this TorqOS as the source a new machine is built from.
      // lib.genAttrs (map (s: "image-${s}") linuxSystems) (name:
        self.lib.mkSystem {
          system = lib.removePrefix "image-" name;
          modules = [ ./hosts/image { torq.setup.source = self.sourceInfo; } ];
        });

      # ── Template for the per-machine config repo the broker manages ─────────
      templates.config = {
        path = ./templates/config;
        description = "TorqOS per-machine config repo (lives at /var/lib/torq/config)";
      };

      # ── Tests ────────────────────────────────────────────────────────────────
      checks = forSystems linuxSystems (pkgs: {
        broker = self.packages.${pkgs.stdenv.hostPlatform.system}.torq-broker;
        trust-loop = import ./tests/trust-loop.nix { inherit pkgs self; };
        tree = import ./tests/tree.nix { inherit pkgs self; };
        accounts = import ./tests/accounts.nix { inherit pkgs self; };
        image = import ./tests/image.nix { inherit pkgs self; };
      });

      # ── Dev shell (works on the Mac too) ─────────────────────────────────────
      devShells = forSystems allSystems (pkgs: {
        default = pkgs.mkShell ({
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer git nixfmt-rfc-style ];
        } // self.packages.${pkgs.stdenv.hostPlatform.system}.torq.filterEnv);
      });
    };
}
