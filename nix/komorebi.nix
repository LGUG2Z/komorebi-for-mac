{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.komorebi;

  applicationsFile = pkgs.fetchurl {
    url = "https://raw.githubusercontent.com/LGUG2Z/komorebi-application-specific-configuration/${cfg.applicationsJsonCommit}/applications.mac.json";
    sha256 = cfg.applicationsJsonSha256;
  };

  finalConfig =
    cfg.config
    // lib.optionalAttrs cfg.enableApplicationsConfiguration {
      app_specific_configuration_path = "${applicationsFile}";
    };

  configFile = pkgs.writeText "komorebi.json" (builtins.toJSON finalConfig);

  barConfigFile = lib.mkIf (cfg.bar.config != { }) (
    pkgs.writeText "komorebi.bar.json" (builtins.toJSON cfg.bar.config)
  );

  exampleConfig = {
    cross_monitor_move_behaviour = "Insert";
    default_workspace_padding = 15;
    default_container_padding = 15;
    resize_delta = 100;
    border = true;
    border_width = 7;
    border_offset = 5;
    theme = {
      palette = "Base16";
      name = "Ashes";
      unfocused_border = "Base03";
      bar_accent = "Base0D";
    };
    monitors = [
      {
        workspaces = [
          {
            name = "I";
            layout = "BSP";
          }
          {
            name = "II";
            layout = "VerticalStack";
          }
          {
            name = "III";
            layout = "HorizontalStack";
          }
          {
            name = "IV";
            layout = "UltrawideVerticalStack";
          }
          {
            name = "V";
            layout = "Rows";
          }
          {
            name = "VI";
            layout = "Grid";
          }
          {
            name = "VII";
            layout = "RightMainVerticalStack";
          }
        ];
      }
    ];
  };

  exampleBarConfig = {
    monitor = 0;
    font_family = "JetBrainsMono Nerd Font";
    height = 30;
    theme = {
      palette = "Base16";
      name = "Ashes";
      accent = "Base0D";
    };
    left_widgets = [
      {
        Komorebi = {
          workspaces = {
            enable = true;
            hide_empty_workspaces = false;
          };
          layout = {
            enable = true;
          };
          focused_window = {
            enable = true;
            show_icon = true;
          };
        };
      }
    ];
    right_widgets = [
      {
        Update = {
          enable = true;
        };
      }
      {
        Storage = {
          enable = true;
        };
      }
      {
        Memory = {
          enable = true;
        };
      }
      {
        Network = {
          enable = true;
          show_activity = true;
          show_total_activity = true;
        };
      }
      {
        Date = {
          enable = true;
          format = "DayDateMonthYear";
        };
      }
      {
        Time = {
          enable = true;
          format = "TwentyFourHour";
        };
      }
    ];
  };
in
{
  options.services.komorebi = {
    enable = lib.mkEnableOption "Whether to enable the komorebi tiling window manager.";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.komorebi;
      defaultText = lib.literalExpression "pkgs.komorebi";
      description = "The komorebi package to use.";
    };

    applicationsJsonCommit = lib.mkOption {
      type = lib.types.str;
      default = "4581c2f6f8a861864a5ddb03f5aa7adfce93861e";
      description = ''
        The Git commit hash to fetch applications.mac.json from.
        Update this to get newer application rules.
      '';
    };

    applicationsJsonSha256 = lib.mkOption {
      type = lib.types.str;
      default = lib.fakeSha256;
      example = "sha256-gfYubJpVEU6vxdd/vqujQhQX8mdAEE9ImugqdIyTQKk=";
      description = ''
        The SHA256 hash of the applications.mac.json file.
        Nix will tell you the correct hash on first build.
      '';
    };

    config = lib.mkOption {
      type = lib.types.attrs;
      default = exampleConfig;
      description = ''
        Configuration for komorebi, written as a Nix attribute set.
        This will be converted to JSON and passed via --config.
        See https://lgug2z.github.io/komorebi/ for configuration options.
      '';
    };

    bar = {
      enable = lib.mkEnableOption "komorebi-bar status bar";

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.komorebi-bar;
        defaultText = lib.literalExpression "pkgs.komorebi-bar";
        description = "The komorebi-bar package to use.";
      };

      config = lib.mkOption {
        type = lib.types.attrs;
        default = exampleBarConfig;
        description = ''
          Configuration for komorebi-bar, written as a Nix attribute set.
          This will be converted to JSON.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ] ++ lib.optional cfg.bar.enable cfg.bar.package;

    launchd.user.agents.komorebi = {
      serviceConfig = {
        ProgramArguments = [
          "${cfg.package}/bin/komorebi"
        ]
        ++ lib.optionals (cfg.config != { }) [
          "--config"
          "${configFile}"
        ];
        KeepAlive = true;
        RunAtLoad = true;
        EnvironmentVariables = {
          PATH = "${cfg.package}/bin:${config.environment.systemPath}";
        };
        StandardOutPath = "/tmp/komorebi.log";
        StandardErrorPath = "/tmp/komorebi.err";
      };
      managedBy = "services.komorebi.enable";
    };

    launchd.user.agents.komorebi-bar = lib.mkIf cfg.bar.enable {
      serviceConfig = {
        ProgramArguments = [
          "${cfg.bar.package}/bin/komorebi-bar"
        ]
        ++ lib.optionals (cfg.bar.config != { }) [
          "--config"
          "${barConfigFile}"
        ];
        KeepAlive = true;
        RunAtLoad = true;
        EnvironmentVariables = {
          PATH = "${cfg.bar.package}/bin:${config.environment.systemPath}";
        };
        StandardOutPath = "/tmp/komorebi-bar.log";
        StandardErrorPath = "/tmp/komorebi-bar.err";
      };
      managedBy = "services.komorebi.bar.enable";
    };
  };
}
