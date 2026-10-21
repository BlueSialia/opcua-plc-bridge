{
  description = "Development environment for the OPC UA PLC bridge";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { nixpkgs, flake-utils, rust-overlay, ... }:
    let
      mise = builtins.fromTOML (builtins.readFile ./mise.toml);
      rustVersion = mise.tools.rust;
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
      in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rust-bin.stable.${rustVersion}.default
            cargo-deny
            cargo-machete
            git-cliff
            clang
            cmake
            gcc
            git
            llvmPackages.libclang
            pkg-config
            python3
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          shellHook = ''
            export PATH="$HOME/.local/share/mise/shims:$PATH"
            export CC="${pkgs.gcc}/bin/gcc"
            export CXX="${pkgs.gcc}/bin/g++"
          '';
        };
      });
}
