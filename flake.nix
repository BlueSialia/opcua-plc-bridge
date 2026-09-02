{
  description = "Development environment for the OPC UA PLC bridge";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo-machete
            clang
            cmake
            gcc
            git
            llvmPackages.libclang
            pkg-config
          ];

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          shellHook = ''
            export PATH="$HOME/.local/share/mise/shims:$PATH"
          '';
        };
      });
}
