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
          # NoCC, and Apple's clang: nixpkgs' cc wrapper pins apple-sdk 14.4 and
          # a linker that cannot read the macOS 26+ SDK, which SpeechAnalyzer
          # (kikimimic-asr-apple) needs.
          default = pkgs.mkShellNoCC {
            SDKROOT = "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk";
            CC = "/usr/bin/clang";
            CXX = "/usr/bin/clang++";
            CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER = "/usr/bin/clang";
            CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER = "/usr/bin/clang";

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
