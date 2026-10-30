{
  lib,
  rustPlatform,
  cmake,
  pkg-config,
  makeWrapper,
  copyDesktopItems,
  makeDesktopItem,
  libxkbcommon,
  wayland,
  libX11,
  libXcursor,
  libXi,
  libXrandr,
  xdg-utils,
}:
let
  manifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  runtimeLibraries = [
    libxkbcommon
    wayland
    libX11
    libXcursor
    libXi
    libXrandr
  ];
in
rustPlatform.buildRustPackage {
  pname = manifest.package.name;
  version = manifest.package.version;
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  buildFeatures = [ "gui" ];
  nativeBuildInputs = [
    cmake
    pkg-config
    makeWrapper
    copyDesktopItems
  ];
  desktopItems = [
    (makeDesktopItem {
      name = "github-notifications";
      desktopName = "GitHub Notifications";
      comment = "Read your local GitHub notification inbox";
      exec = "github-notifications inbox";
      icon = "preferences-system-notifications";
      categories = [
        "Network"
        "Development"
      ];
      terminal = false;
    })
  ];
  postInstall = ''
    # The software-rendered Iced backend dynamically loads its windowing
    # libraries. Scope their lookup paths to this executable, not the session.
    wrapProgram "$out/bin/github-notifications" \
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibraries}" \
      --prefix PATH : "${lib.makeBinPath [ xdg-utils ]}"
  '';
  passthru = { inherit runtimeLibraries; };
  meta = {
    description = manifest.package.description;
    homepage = "https://github.com/warrenfalk/github-notifications";
    mainProgram = "github-notifications";
    platforms = lib.platforms.linux;
  };
}
