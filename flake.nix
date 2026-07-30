{
  description = "http-share — minimal HTTP(S) file sharing utility";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        http-share = pkgs.rustPlatform.buildRustPackage {
          pname = "http-share";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # Zero external crate deps; pure std.
          meta = with pkgs.lib; {
            description = "Minimal HTTP(S) file sharing utility for ad-hoc transfers";
            license = licenses.mit;
            mainProgram = "http-share";
          };
        };
      in {
        packages.default = http-share;
        packages.http-share = http-share;

        apps.default = {
          type = "app";
          program = "${http-share}/bin/http-share";
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
          ];
        };

        checks.http-share = http-share;

        formatter = pkgs.nixpkgs-fmt;
      });
}
