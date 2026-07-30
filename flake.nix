# SPDX-FileCopyrightText: 2026 Ingo Ruhnke <grumbel@gmail.com>
# SPDX-License-Identifier: GPL-3.0-or-later

{
  description = "http-share — minimal HTTP(S) file sharing utility";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        lib = pkgs.lib;
        # Source of truth: top-level VERSION (e.g. "0.1.0-dev").
        versionBase = lib.strings.removeSuffix "\n" (builtins.readFile ./VERSION);
        gitRev = "${self.shortRev or self.dirtyShortRev or "dirty"}";
        version = "${versionBase}+g${gitRev}";

        http-share = pkgs.rustPlatform.buildRustPackage {
          pname = "http-share";
          inherit version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # Embed the flake-expanded version (with +g<rev>) into the binary.
          HTTP_SHARE_VERSION_OVERRIDE = version;
          meta = with lib; {
            description = "Minimal HTTP(S) file sharing utility for ad-hoc transfers";
            license = licenses.gpl3Plus;
            mainProgram = "http-share";
          };
        };
      in {
        packages.default = http-share;
        packages.http-share = http-share;

        apps.default = {
          type = "app";
          program = "${http-share}/bin/http-share";
          meta = {
            description = "Minimal HTTP(S) file sharing utility for ad-hoc transfers";
          };
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

        checks = {
          http-share = http-share;
          # SPDX / REUSE compliance (headers + REUSE.toml annotations)
          reuse = pkgs.runCommand "reuse-lint" {
            nativeBuildInputs = [ pkgs.reuse ];
          } ''
            reuse --root ${./.} lint
            touch "$out"
          '';
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
