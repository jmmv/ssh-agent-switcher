{ lib, buildGoModule, ... } : buildGoModule {
  name = "ssh-agent-switcher";
  src = ./.;
  vendorHash = null;
}
