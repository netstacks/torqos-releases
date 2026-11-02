# TorqOS's releases (docs/DECISIONS.md, ADR-028): the key they are signed with, for torqcli. A
# machine's update moves it only to a commit of the releases that this key signed
# (`request system update`); the same key signs each release's checksums (docs/INSTALL.md).
{ ... }:
{
  environment.etc."torq/release-signers".source = ../release/allowed_signers;
}
