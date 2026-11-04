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
      # The image as the ISO installer carries it (ADR-031, hosts/installer/payload.nix).
      payload = pkgs: import ./hosts/installer/payload.nix {
        inherit pkgs;
        image = self.nixosConfigurations."image-${pkgs.stdenv.hostPlatform.system}".config.system.build.images.qemu-efi;
      };
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
        # The ISO installer (ADR-031): `nix build .#iso`, for a USB stick or disc; it installs the image
        # above on a real machine's disk, with that machine's hardware.
        iso = let iso = self.nixosConfigurations."installer-${pkgs.stdenv.hostPlatform.system}".config.system.build.isoImage; in
          pkgs.runCommand "torqos-iso-${pkgs.stdenv.hostPlatform.system}" { } "mkdir $out; ln -s ${iso}/iso/*.iso $out/";
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
        })
      # The installer's own system (ADR-031), carrying the image of the same platform.
      // lib.genAttrs (map (s: "installer-${s}") linuxSystems) (name:
        let system = lib.removePrefix "installer-" name; in
        self.lib.mkSystem {
          inherit system;
          modules = [ ./hosts/installer { torq.installer.payload = payload nixpkgs.legacyPackages.${system}; } ];
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
        # How a machine starts: a real EFI partition and its boot menu (ADR-033).
        boot = import ./tests/boot.nix { inherit pkgs self; };
        tree = import ./tests/tree.nix { inherit pkgs self; };
        accounts = import ./tests/accounts.nix { inherit pkgs self; };
        ssh = import ./tests/ssh.nix { inherit pkgs self; };
        image = import ./tests/image.nix { inherit pkgs self; };
        # Nothing nixpkgs marks unfree in what TorqOS distributes (ADR-039), for every platform it
        # releases (evaluation only, so x86_64's CI checks aarch64's too). nixpkgs refuses to
        # evaluate an unfree package unless its configuration lets it through, so the check is
        # that nothing does: none of nixpkgs' settings that let one through, in the image and the
        # ISO or any specialisation of them, no package set from outside (`nixpkgs.pkgs`), and no
        # second nixpkgs imported in TorqOS's modules (which would have settings of its own).
        # What it cannot see is a package with no licence check at all (a vendor's file fetched
        # as it is); redistributable firmware nixpkgs counts as free (`enableRedistributableFirmware`).
        # And it checks itself: each way of letting one through, planted in the image, is caught.
        free =
          let
            # What a configuration (`config`, evaluated) lets through that nixpkgs marks unfree.
            allows = config:
              let n = config.nixpkgs.config; u = config.torq.unfree; in
              lib.optional (n.allowUnfree or false) "nixpkgs.config.allowUnfree"
              ++ lib.optional (n ? allowUnfreePredicate) "an allowUnfreePredicate (torq.unfree: ${if u.all then "all" else if u.packages == [ ] then "none; set elsewhere" else lib.concatStringsSep ", " u.packages})"
              ++ lib.optional ((n.allowUnfreePackages or [ ]) != [ ]) "nixpkgs.config.allowUnfreePackages"
              ++ lib.optional ((n.allowlistedLicenses or [ ]) != [ ] || (n.whitelistedLicenses or [ ]) != [ ]) "nixpkgs.config.allowlistedLicenses"
              ++ lib.optional (n ? handleEvalIssue) "nixpkgs.config.handleEvalIssue"
              ++ lib.concatLists (lib.mapAttrsToList (name: sp: map (a: "its specialisation ${name}: ${a}") (allows sp.configuration)) config.specialisation);
            ofSystem = c: allows c.config ++ lib.optional c.options.nixpkgs.pkgs.isDefined "a package set from outside (nixpkgs.pkgs)";
            shipped = lib.concatMap (sys: [
              { what = "the image (${sys})"; c = self.nixosConfigurations."image-${sys}"; }
              { what = "the ISO installer (${sys})"; c = self.nixosConfigurations."installer-${sys}"; }
            ]) linuxSystems;
            found = lib.concatMap ({ what, c }: map (a: "${what} lets through ${a}") (ofSystem c)) shipped;
            image = self.nixosConfigurations."image-${pkgs.stdenv.hostPlatform.system}";
            planted = map (m: image.extendModules { modules = [ m ]; }) [
              { torq.unfree.packages = [ "claude-code" ]; }
              { torq.unfree.all = true; }
              { nixpkgs.config.allowUnfree = true; }
              { nixpkgs.config.allowUnfreePackages = [ "claude-code" ]; }
              { nixpkgs.config.allowlistedLicenses = [ lib.licenses.unfree ]; }
              { nixpkgs.config.handleEvalIssue = _: _: true; }
              { specialisation.planted.configuration.torq.unfree.packages = [ "claude-code" ]; }
              { nixpkgs.pkgs = pkgs; }
            ];
            missed = lib.filter (c: ofSystem c == [ ]) planted;
          in
          assert lib.assertMsg (found == [ ]) "Unfree software could reach what TorqOS distributes (ADR-039): ${lib.concatStringsSep "; " found}";
          assert lib.assertMsg (missed == [ ]) "The unfree check misses ${toString (lib.length missed)} of the ways planted in the image";
          pkgs.runCommand "torq-free" { } ''
            # No second nixpkgs in TorqOS's own Nix (modules, hosts, its two packages): one imported
            # has settings of its own, out of the check above. So nixpkgs' tree is not reached at all
            # (`pkgs.path`, `<nixpkgs>`, NixOS's modules' parent), however it is spelled. And no
            # licence set but TorqOS's own packages': a package relabelled free passes nixpkgs' check.
            src="${./modules} ${./hosts} ${./broker/package.nix} ${./cli/package.nix}"
            if grep -rnE 'pkgs\.path|<nixpkgs>|import[[:space:]]*\(?[[:space:]]*(inputs\.)?nixpkgs([[:space:]]|\))|modulesPath[[:space:]]*\+[[:space:]]*"/\.\.|NIXPKGS_ALLOW' $src; then
              echo "nixpkgs' tree is reached above: a nixpkgs of its own would be out of the check (ADR-039)."
              exit 1
            fi
            if grep -rnE '(^|[^.[:alnum:]])license[[:space:]]*=' $src | grep -vE 'package\.nix:[0-9]+:[[:space:]]*license = lib\.licenses\.asl20;$'; then
              echo "A licence is set above: a package relabelled is out of nixpkgs' check (ADR-039)."
              exit 1
            fi
            echo "Nothing lets an unfree package into the image or the ISO, ${lib.concatStringsSep " or " linuxSystems} (ADR-039); each of ${toString (lib.length planted)} ways planted is caught." > $out
          '';
      } // lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
        # The ISO installer (ADR-031): installed and booted on x86_64 only; aarch64's ISO is built, not run.
        installer = import ./tests/installer.nix { inherit pkgs self; };
        # Thin, kept so (ADR-032): a new machine's own system — the image's, what everyone installs —
        # stays within its budget. Lowered as TorqOS sheds what it does not need, never raised to
        # make a change fit without saying why in docs/DECISIONS.md. x86_64's; aarch64's is CI's.
        size =
          let
            budget = 1280;
            top = self.nixosConfigurations.image-x86_64-linux.config.system.build.toplevel;
            info = pkgs.closureInfo { rootPaths = [ top ]; };
          in
          pkgs.runCommand "torq-size" { } ''
            mb=$(( $(cat ${info}/total-nar-size) / 1000000 ))
            echo "A new TorqOS machine's system: $mb MB, $(wc -l < ${info}/store-paths) store paths (budget: ${toString budget} MB)."
            if [ "$mb" -gt ${toString budget} ]; then
              echo "Over its budget by $(( mb - ${toString budget} )) MB. What grew: nix path-info -rsS on it, nix why-depends (ADR-032)."
              exit 1
            fi
            echo "$mb" > $out
          '';
      });

      # ── Dev shell (works on the Mac too) ─────────────────────────────────────
      devShells = forSystems allSystems (pkgs: {
        default = pkgs.mkShell ({
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer git nixfmt-rfc-style ];
        } // self.packages.${pkgs.stdenv.hostPlatform.system}.torq.filterEnv);
      });
    };
}
