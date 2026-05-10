{
  description = "agent_info_probe_rs — Rust shim over LogosAPI (Phase B)";

  inputs = {
    logos-nix.url = "github:logos-co/logos-nix";
    nixpkgs.follows = "logos-nix/nixpkgs";
    logos-cpp-sdk.url = "github:logos-co/logos-cpp-sdk";
  };

  outputs = { self, nixpkgs, logos-nix, logos-cpp-sdk }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f {
        inherit system;
        pkgs = import nixpkgs { inherit system; };
        sdkSrc = logos-cpp-sdk.outPath;
      });
    in
    {
      packages = forAllSystems ({ pkgs, sdkSrc, ... }: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "agent_info_probe_rs";
          version = "0.0.1";
          src = ./.;

          # Bindgen needs clang and the libclang shared lib visible at
          # bindgen build-script runtime.
          nativeBuildInputs = with pkgs; [
            cmake
            ninja
            pkg-config
            qt6.wrapQtAppsHook
            rustPlatform.bindgenHook
          ];
          buildInputs = with pkgs;
            (with pkgs.qt6; [ qtbase qtremoteobjects ])
            ++ [ boost openssl nlohmann_json ];

          # cargo-build invokes our build.rs which `cmake`s the shim
          # subdir. Pass the SDK source path through env.
          LOGOS_CPP_SDK_DIR = sdkSrc;

          # buildRustPackage's default cmake / ninja setupHooks would
          # step on our build.rs's cmake-rs invocation (which spawns
          # its own cmake + builder). Disable both so the Rust build
          # owns the build pipeline.
          dontUseCmakeConfigure = true;
          dontUseCmakeBuild = true;
          dontUseNinjaBuild = true;
          dontUseNinjaInstall = true;

          cargoLock.lockFile = ./Cargo.lock;

          # Keep the build deterministic; experiments don't need
          # offline-network shenanigans.
          doCheck = false;

          meta.description = "Rust crate calling LogosAPI through a C++ shim";
        };
      });

      apps = forAllSystems ({ system, ... }: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/agent_info_probe_rs";
        };
      });

      devShells = forAllSystems ({ pkgs, sdkSrc, ... }: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cmake ninja pkg-config rustc cargo rustfmt clippy
            rustPlatform.bindgenHook
          ] ++ (with pkgs.qt6; [ qtbase qtremoteobjects ])
            ++ [ boost openssl nlohmann_json ];
          shellHook = ''
            export LOGOS_CPP_SDK_DIR=${sdkSrc}
          '';
        };
      });
    };
}
