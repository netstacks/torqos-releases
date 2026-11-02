# The machine's identity, as data: `system host-name` and `system time-zone` in the configuration tree
# (docs/CLI-DESIGN.md), stored in the config repo as `system/host.json` (system tier):
#
#   { "host-name": "torq-utm", "time-zone": "Europe/Berlin" }
#
# Both are optional (leave a key out; null is refused, not read as "unset"). This module is the
# authority on what is valid, whoever wrote the file (torqcli, the agent, a raw diff): anything else
# fails evaluation, so the broker's rebuild fails and the change is undone. The rules are in
# host-rules.nix. Changing the host name never changes which configuration the broker builds
# (`nixosConfigurations.torq`, torq.broker.configName).
#
# A JSON object with a key twice keeps the last value: what a person approving the diff reads first
# is not necessarily what applies. torqcli never writes such a file.
{ config, lib, pkgs, ... }:

let
  dir = config.torq.packages.configDir;
  file = dir + "/system/host.json";
  where = "torq: system/host.json";
  rules = import ./host-rules.nix { inherit lib; inherit (pkgs) tzdata; };

  known = [ "host-name" "time-zone" ];

  settings =
    let
      v = builtins.addErrorContext "while reading ${where}" (lib.importJSON file);
      unknown = lib.subtractLists known (lib.attrNames v);
    in
    if !(builtins.isAttrs v) then
      throw "${where} must be a JSON object with \"host-name\" and/or \"time-zone\""
    else if unknown != [ ] then
      throw "${where}: \"${lib.head unknown}\" is not a setting (it holds ${lib.concatStringsSep ", " known})"
    else if v ? host-name && !(rules.hostNameOk v.host-name) then
      throw "${where}: host-name ${builtins.toJSON v.host-name} must be one label of letters, digits and hyphens, at most 63 characters, not starting or ending with a hyphen"
    else if v ? time-zone && !(rules.timeZoneOk v.time-zone) then
      throw "${where}: time-zone ${builtins.toJSON v.time-zone} is not a time zone tzdata lists (e.g. Europe/Berlin, America/New_York, UTC)"
    else
      { host = v.host-name or null; zone = v.time-zone or null; };

  present = dir != null && builtins.pathExists file;
in
{
  config = lib.mkMerge [
    (lib.mkIf present {
      # Normal priority: setting the host name in Nix as well is a conflict, reported, not silently
      # overridden one way or the other.
      networking.hostName = lib.mkIf (settings.host != null) settings.host;
      time.timeZone = lib.mkIf (settings.zone != null) settings.zone;
    })
    # The running machine has the name its configuration gives it at every switch, not only from
    # the next start: NixOS writes /etc/hostname, which systemd reads when the machine boots — a
    # commit (first-boot setup's too) left the old name until a restart, while the configuration
    # showed the new one.
    (lib.mkIf (config.networking.hostName != "") {
      system.activationScripts.torqHostname = {
        deps = [ "etc" ];
        text = "printf %s ${lib.escapeShellArg config.networking.hostName} > /proc/sys/kernel/hostname";
      };
    })
    {
      torq.schema = {
        "system host-name" = {
          kind = "leaf";
          help = "This machine's name on the network (one label: letters, digits, hyphens)";
          file = "system/host.json";
          key = "host-name";
          type = "host-name";
        };
        "system time-zone" = {
          kind = "leaf";
          help = "Local time zone, e.g. Europe/Berlin (the ledger is always UTC)";
          file = "system/host.json";
          key = "time-zone";
          values = lib.genAttrs rules.timeZones (_: "");
        };
      };
    }
  ];
}
