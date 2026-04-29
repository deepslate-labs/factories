{
  description = "Factories development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    nix-rust-wrangler = {
      url = "github:Janrupf/nix-rust-wrangler";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, nix-rust-wrangler, rust-overlay }:
  let
      rustOverlayInstance = (import rust-overlay);
  in
  (flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = import nixpkgs {
        inherit system;
        config = {
          allowUnfree = true;
        };
        overlays = [ rustOverlayInstance nix-rust-wrangler.overlays.default ];
      };

      nix-rust-wrangler-lib = nix-rust-wrangler.lib.${system};

      toolchainCollection = nix-rust-wrangler-lib.mkToolchainCollection [
        (nix-rust-wrangler-lib.deriveToolchainInstance (
          pkgs.rust-bin.stable.latest.default.override {
            extensions = [ "rust-src" "clippy" "rust-analyzer" ];
          }
        ))
        (nix-rust-wrangler-lib.deriveToolchainInstance (
          pkgs.rust-bin.nightly.latest.default.override {
            extensions = [ "rust-src" "clippy" "rust-analyzer" "miri-preview" ];
          }
        ))
      ];
    in
    {
      devShells.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          # Tools
          pkg-config
          valgrind
          stdenv.cc
          gnumake
          perf
          pkgs.nix-rust-wrangler
  
          # Networking
          openssl
          protobuf
          protoc-gen-prost

          # Media processing
          ffmpeg-full
          alsa-lib
          libopus
        ];

        env = {
          NIX_RUST_WRANGLER_TOOLCHAIN_COLLECTION = toolchainCollection;
          NIX_RUST_WRANGLER_INSIDE_NIX_DEVELOP = "1";

          LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";

          BINDGEN_EXTRA_CLANG_ARGS = let
            stdenv = pkgs.stdenv;
          in pkgs.lib.strings.concatStringsSep " " ((map builtins.readFile [
            "${stdenv.cc}/nix-support/libc-crt1-cflags"
            "${stdenv.cc}/nix-support/libc-cflags"
            "${stdenv.cc}/nix-support/cc-cflags"
            "${stdenv.cc}/nix-support/libcxx-cxxflags"
          ]) ++ (
            pkgs.lib.lists.optional
              stdenv.cc.isClang
              "-idirafter ${stdenv.cc.cc}/lib/clang/${pkgs.lib.getVersion stdenv.cc.cc}/include"
          ) ++ (
            pkgs.lib.lists.optional
            stdenv.cc.isGNU
            "-isystem ${stdenv.cc.cc}/include/c++/${pkgs.lib.getVersion stdenv.cc.cc}
             -isystem ${stdenv.cc.cc}/include/c++/${pkgs.lib.getVersion stdenv.cc.cc}/${stdenv.hostPlatform.config}
             -idirafter ${stdenv.cc.cc}/lib/gcc/${stdenv.hostPlatform.config}/${pkgs.lib.getVersion stdenv.cc.cc}/include"
          ));
        };
      };
    }
  ));
}
