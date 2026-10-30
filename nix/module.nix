{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.github-notifications;
in
{
  options.services.github-notifications = {
    enable = lib.mkEnableOption "GitHub notifications in a desktop user's graphical session";
    user = lib.mkOption {
      type = lib.types.str;
      description = "User whose graphical session runs the notification collector.";
    };
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "github-notifications.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "GitHub Notifications package to install and run.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    systemd.user.services.github-notifications = {
      description = "GitHub notification collector";
      wantedBy = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      after = [ "graphical-session-pre.target" ];
      unitConfig = {
        ConditionUser = cfg.user;
        StartLimitIntervalSec = 0;
      };
      serviceConfig = {
        Type = "exec";
        ExecStart = "${lib.getExe cfg.package} watch";
        WorkingDirectory = "%h";
        StandardInput = "null";
        Restart = "on-failure";
        RestartSec = 30;
        UMask = "0077";
      };
    };
  };
}
