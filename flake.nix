{
  description = "repoweave dev shell — all ecosystem tools for e2e integration testing";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            # Rust (repoweave itself)
            cargo
            rustc
            rustfmt
            clippy

            # Go
            go

            # Node (npm ships with nodejs)
            nodejs
            pnpm

            # Python
            python3
            uv

            # Git (tests need it)
            git

            # gita (multi-repo dashboard)
            gita
          ];

          shellHook = ''
            echo "repoweave dev shell — all ecosystem tools available"
            echo "  cargo $(cargo --version 2>/dev/null | cut -d' ' -f2)"
            echo "  go    $(go version 2>/dev/null | cut -d' ' -f3)"
            echo "  node  $(node --version 2>/dev/null)"
            echo "  npm   $(npm --version 2>/dev/null)"
            echo "  pnpm  $(pnpm --version 2>/dev/null)"
            echo "  uv    $(uv --version 2>/dev/null | cut -d' ' -f2)"
            echo "  gita  $(gita --version 2>/dev/null || echo 'available')"
          '';
        };
      }
    );
}
