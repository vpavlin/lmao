{
  description = "agent_info_probe — Remote-mode LogosAPI consumer spike";

  inputs = {
    # Follow the workspace's nixpkgs / Qt pin so the probe links against
    # the same Qt6 the SDK + Basecamp + modules are built with. Pinning
    # an independent nixpkgs would let us silently drift onto a
    # different Qt and produce QtRO ABI mismatches at runtime that look
    # like generic "registry host not reachable" failures.
    logos-nix.url = "github:logos-co/logos-nix";
    nixpkgs.follows = "logos-nix/nixpkgs";

    # The SDK is consumed as a source tree — we compile its `.cpp`s into
    # the probe directly so this stays a single self-contained binary.
    # Once the Phase B Rust shim lands, that part will use the SDK as a
    # CMake package the way other modules do.
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
        default = pkgs.stdenv.mkDerivation {
          pname = "agent_info_probe";
          version = "0.0.1";
          src = ./.;

          nativeBuildInputs = with pkgs; [ cmake ninja qt6.wrapQtAppsHook pkg-config ];
          buildInputs = with pkgs;
            (with pkgs.qt6; [ qtbase qtremoteobjects ])
            # Transitive deps of the SDK's `logos_sdk` static target —
            # Boost.System (asio's error_code), OpenSSL, nlohmann_json.
            # Without these the SDK's own CMakeLists fails at
            # find_package time before we even reach our target.
            ++ [ boost openssl nlohmann_json ];

          cmakeFlags = [
            "-DLOGOS_CPP_SDK_DIR=${sdkSrc}"
            "-DCMAKE_BUILD_TYPE=Release"
            "-G" "Ninja"
          ];

          installPhase = ''
            mkdir -p $out/bin
            cp agent_info_probe $out/bin/
          '';

          meta = with pkgs.lib; {
            description = "Remote-mode LogosAPI consumer probe — calls agent.info() against a running logoscore --mode 0";
            platforms = platforms.unix;
          };
        };
      });

      apps = forAllSystems ({ system, ... }: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/agent_info_probe";
        };
      });

      devShells = forAllSystems ({ pkgs, sdkSrc, ... }: {
        # `nix develop` to iterate on main.cpp without rebuilding the
        # nix derivation each time. LOGOS_CPP_SDK_DIR is preset so a
        # plain `cmake -B build && cmake --build build` works.
        default = pkgs.mkShell {
          packages = with pkgs; [ cmake ninja pkg-config ]
            ++ (with pkgs.qt6; [ qtbase qtremoteobjects ])
            ++ [ boost openssl nlohmann_json ];
          shellHook = ''
            export LOGOS_CPP_SDK_DIR=${sdkSrc}
          '';
        };
      });
    };
}
