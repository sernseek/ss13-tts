{
  description = "SS13-compatible cloud TTS adapter";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      devShells = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clang
              clippy
              pkg-config
              rustc
              rust-analyzer
              rustfmt
            ];

            RUST_BACKTRACE = "1";
          };
        });
    };
}
