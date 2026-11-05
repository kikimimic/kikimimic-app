{
  description = "Kikimimic development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            SDKROOT = "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk";

            packages = [
              # rustup, not nixpkgs' cargo/rustc: rust-toolchain.toml pins the
              # version GPUI needs, and rustup is what reads it.
              pkgs.rustup
              pkgs.cargo-flamegraph
              pkgs.pkg-config
              pkgs.cmake
              pkgs.uv
            ];

            shellHook = ''
              echo "Kikimimic dev shell: cargo $(cargo --version | awk '{print $2}')"
            '';
          };
        }
      );
    };
}
