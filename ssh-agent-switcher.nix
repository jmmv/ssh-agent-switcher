{
  lib,
  rustPlatform,
  ...
}:
rustPlatform.buildRustPackage {
  pname = "ssh-agent-switcher";
  version = "1.0.1";
  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;
}
